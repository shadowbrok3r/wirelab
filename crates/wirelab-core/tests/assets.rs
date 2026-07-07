//! Every shipped board profile and component definition must load cleanly.

use std::path::PathBuf;

use wirelab_core::library::{Library, lint_board, lint_component};

fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

#[test]
fn all_assets_load_and_lint_clean() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    assert!(!lib.boards.is_empty(), "no board profiles found");
    assert!(!lib.components.is_empty(), "no components found");

    let mut problems = Vec::new();
    for board in lib.boards.values() {
        for p in lint_board(board) {
            problems.push(format!("board {}: {p}", board.id));
        }
    }
    for comp in lib.components.values() {
        for p in lint_component(comp) {
            problems.push(format!("component {}: {p}", comp.id));
        }
    }
    assert!(problems.is_empty(), "asset problems:\n{}", problems.join("\n"));
}

/// Shipped examples must parse, reference known assets, lint clean and
/// their scripts must compile; the PWM example must actually emit PWM.
#[test]
fn examples_load_and_run() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let dir = assets.join("examples");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).expect("examples dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        // Users may save their own projects here; count only shipped ones.
        let shipped = path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.len() > 3 && f.as_bytes()[2] == b'-' && f[..2].chars().all(|c| c.is_ascii_digit()));
        if shipped {
            n += 1;
        }
        let project =
            wirelab_core::project::Project::load(&path).unwrap_or_else(|e| {
                panic!("{} does not parse: {e}", path.display());
            });
        for tab in &project.boards {
            let board = lib
                .board(&tab.circuit.board_id)
                .unwrap_or_else(|| panic!("{}: unknown board", path.display()));
            for comp in tab.circuit.components.values() {
                assert!(
                    lib.component(&comp.def_id).is_some(),
                    "{}: unknown component {}",
                    path.display(),
                    comp.def_id
                );
            }
            let nl = wirelab_core::netlist::Netlist::build(&tab.circuit, board, &lib);
            let lints = wirelab_core::validate::validate(&tab.circuit, board, &lib, &nl);
            let errors: Vec<_> = lints
                .iter()
                .filter(|l| matches!(l.severity, wirelab_core::validate::Severity::Error))
                .collect();
            assert!(errors.is_empty(), "{}: {errors:?}", path.display());

            let mut host = wirelab_core::script::ScriptHost::new();
            host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
            host.sync(&tab.circuit, &lib);
            if !tab.flow.nodes.is_empty() {
                let code = wirelab_core::flow::compile(&tab.flow).unwrap_or_else(|e| {
                    panic!("{}: flow does not compile: {e:?}", path.display());
                });
                assert!(
                    host.set_flow_script(Some(&code)),
                    "{}: flow script rejected: {:?}",
                    path.display(),
                    host.errors
                );
            }
            assert!(
                host.errors.is_empty(),
                "{}: script errors {:?}",
                path.display(),
                host.errors
            );
        }
    }
    assert_eq!(n, 19, "expected the nineteen shipped examples");
}

/// The LCD messenger: a press on the C5 desk board sends a message to the
/// Waveshare C6-LCD board, which prints it to the LCD and acks so the desk
/// LED settles green.
#[test]
fn lcd_messenger_round_trip() {
    use wirelab_core::program::Action;
    use wirelab_core::script::ScriptHost;

    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/13-lcd-messenger.wirelab.json"),
    )
    .expect("example parses");
    assert_eq!(project.boards.len(), 2);
    let desk = &project.boards[0];
    let display = &project.boards[1];

    let host_for = |tab: &wirelab_core::project::BoardTab| {
        let profile = lib.board(&tab.circuit.board_id).expect("board");
        let mut h = ScriptHost::new();
        h.set_board(profile.chip.name(), &profile.specs, profile.features.rgb_led_gpio);
        h.sync(&tab.circuit, &lib);
        assert!(h.errors.is_empty(), "{:?}", h.errors);
        for c in h.scripted() {
            h.on_start(c);
        }
        h
    };
    let mut h_desk = host_for(desk);
    let mut h_display = host_for(display);

    let btn = *desk.circuit.components.values().find(|c| c.label == "send_btn").map(|c| &c.id).unwrap();

    // Press on the desk -> the first message addressed to "display", plus a
    // confirming LED-on and buzzer beep.
    let sent = h_desk.on_press(btn);
    let text = sent.iter().find_map(|a| match a {
        Action::BoardMsg { to, text } if to == "display" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(text.as_deref(), Some("hello!"));
    assert!(
        sent.iter().any(|a| matches!(a, Action::CompAction { action, .. } if action == "beep")),
        "the send should chirp the buzzer: {sent:?}"
    );

    // Deliver it to the display (as the app router does): it paints the text
    // to the LCD and acks the sender.
    let mut shown = Vec::new();
    for c in h_display.scripted() {
        shown.extend(h_display.on_board_msg(c, "desk", "hello!"));
    }
    assert!(
        shown.iter().any(|a| matches!(
            a,
            Action::LcdText { text, .. } if text == "hello!"
        )),
        "the message reaches the LCD: {shown:?}"
    );
    let ack = shown.iter().find_map(|a| match a {
        Action::BoardMsg { to, text } if to == "desk" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(ack.as_deref(), Some("shown"));

    // The ack lights the desk RGB green (SetRgb with g > 0).
    let home = h_desk.on_board_msg(btn, "display", "shown");
    assert!(
        home.iter().any(|a| matches!(a, Action::SetRgb { g, .. } if *g > 0)),
        "{home:?}"
    );
}

/// The two-board example: the house button toggles the garage servo over
/// send_board, and the garage acknowledges so the house LED stays truthful.
#[test]
fn house_and_garage_round_trip() {
    use wirelab_core::program::Action;
    use wirelab_core::script::ScriptHost;

    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/12-house-and-garage.wirelab.json"),
    )
    .expect("example parses");
    assert_eq!(project.boards.len(), 2);
    let house = &project.boards[0];
    let garage = &project.boards[1];
    let profile = lib.board(&house.circuit.board_id).expect("board");

    let host_for = |circuit: &wirelab_core::circuit::Circuit| {
        let mut h = ScriptHost::new();
        h.set_board(profile.chip.name(), &profile.specs, profile.features.rgb_led_gpio);
        h.sync(circuit, &lib);
        assert!(h.errors.is_empty(), "{:?}", h.errors);
        for c in h.scripted() {
            h.on_start(c);
        }
        h
    };
    let mut h_house = host_for(&house.circuit);
    let mut h_garage = host_for(&garage.circuit);

    let btn = *house.circuit.components.values().find(|c| c.label == "door_btn").map(|c| &c.id).unwrap();
    let door = *garage.circuit.components.values().find(|c| c.label == "door").map(|c| &c.id).unwrap();

    // Press in the house → "toggle" addressed to the garage.
    let sent = h_house.on_press(btn);
    let toggle = sent.iter().find_map(|a| match a {
        Action::BoardMsg { to, text } if to == "garage" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(toggle.as_deref(), Some("toggle"));

    // Deliver to the garage (as the app router does): servo opens, ack sent.
    let mut reply = Vec::new();
    for c in h_garage.scripted() {
        reply.extend(h_garage.on_board_msg(c, "house", "toggle"));
    }
    assert!(
        reply.iter().any(|a| matches!(
            a,
            Action::CompAction { comp, action, params }
                if *comp == door && action == "set_angle"
                    && params.get("degrees").copied() == Some(90.0)
        )),
        "{reply:?}"
    );
    let ack = reply.iter().find_map(|a| match a {
        Action::BoardMsg { to, text } if to == "house" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(ack.as_deref(), Some("opened"));

    // The ack back home lights the RGB (green = SetRgb with g > 0).
    let home = h_house.on_board_msg(btn, "garage", "opened");
    assert!(
        home.iter().any(|a| matches!(a, Action::SetRgb { g, .. } if *g > 0)),
        "{home:?}"
    );

    // Second toggle closes it again.
    let mut second = Vec::new();
    for c in h_garage.scripted() {
        second.extend(h_garage.on_board_msg(c, "house", "toggle"));
    }
    assert!(second.iter().any(|a| matches!(
        a,
        Action::BoardMsg { to, text } if to == "house" && text == "closed"
    )));
}

/// The security-panel example: its FSM compiles and actually runs — a long
/// keypad hold panics into the alarm state (which drives the PWM siren).
#[test]
fn security_panel_fsm_panics_on_long_press() {
    use wirelab_core::program::Action;

    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/11-security-panel.wirelab.json"),
    )
    .expect("example parses");
    let board = lib.board(&project.circuit.board_id).expect("board");

    let keypad = *project
        .circuit
        .components
        .values()
        .find(|c| c.label == "keypad")
        .map(|c| &c.id)
        .expect("keypad");

    let mut host = wirelab_core::script::ScriptHost::new();
    host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
    let fresh = host.sync(&project.circuit, &lib);
    assert!(fresh.contains(&keypad), "brain script compiled: {:?}", host.errors);
    host.on_start(keypad);

    // Press at t=0, release at t=2000 → a >1 s hold → PANIC → alarm.
    host.tick(0);
    host.on_press(keypad);
    host.tick(2000);
    let actions = host.on_release(keypad);
    assert!(
        actions.iter().any(|a| matches!(a, Action::SetPwm { gpio: 25, .. })),
        "long press should sound the siren on GPIO25: {actions:?}"
    );

    // A tick in the alarm state keeps the siren wailing.
    let tick = host.tick(2100);
    assert!(
        tick.iter().any(|a| matches!(a, Action::SetPwm { gpio: 25, .. })),
        "alarm state drives the siren every tick: {tick:?}"
    );
}

/// The flow example: pressing the button arms the gate, ticks blink the LED.
#[test]
fn flow_blink_example_runs() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/10-flow-blink.wirelab.json"),
    )
    .expect("example parses");
    let btn = *project
        .circuit
        .components
        .values()
        .find(|c| c.label == "btn")
        .map(|c| &c.id)
        .expect("btn");
    let led = *project
        .circuit
        .components
        .values()
        .find(|c| c.label == "red_led")
        .map(|c| &c.id)
        .expect("red_led");

    let code = wirelab_core::flow::compile(&project.flow).expect("flow compiles");
    let mut host = wirelab_core::script::ScriptHost::new();
    host.sync(&project.circuit, &lib);
    assert!(host.set_flow_script(Some(&code)), "{:?}", host.errors);
    host.on_start(wirelab_core::script::FLOW_ID);

    host.tick(0);
    // Untoggled: ticks pass but the gate is shut.
    let idle = host.tick(500);
    assert!(idle.is_empty(), "{idle:?}");
    // Press arms the gate; the next tick toggles the LED.
    host.on_press(btn);
    let armed = host.tick(1000);
    assert!(
        armed.iter().any(|a| matches!(
            a,
            wirelab_core::program::Action::CompAction { comp, action, .. }
                if *comp == led && action == "toggle"
        )),
        "{armed:?}"
    );
}

/// The reaction-timer example: a press arms it, the tick lights the LED,
/// the next press logs a reaction time and beeps.
#[test]
fn reaction_timer_example_round_trips() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/04-reaction-timer.wirelab.json"),
    )
    .expect("example parses");
    let board = lib.board(&project.circuit.board_id).expect("board");

    // Wiring must bind everything: button in, LED + buzzer out.
    let nl = wirelab_core::netlist::Netlist::build(&project.circuit, board, &lib);
    let (_msgs, bindings) =
        wirelab_core::engine::plan_setup(&project.circuit, board, &lib, &nl);
    assert_eq!(bindings.outputs.len(), 2, "{:?}", bindings.warnings);
    assert_eq!(bindings.inputs.len(), 1);

    let btn = *project
        .circuit
        .components
        .values()
        .find(|c| c.label == "btn")
        .map(|c| &c.id)
        .expect("btn");
    let mut host = wirelab_core::script::ScriptHost::new();
    host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
    host.sync(&project.circuit, &lib);
    assert!(host.errors.is_empty(), "{:?}", host.errors);

    host.set_world(wirelab_core::script::World { now_ms: 1000, ..Default::default() });
    host.on_start(btn);
    host.on_press(btn); // arms with a deadline <= 1000 + 2600
    let lit = host.tick(1000 + 2700);
    assert!(
        lit.iter().any(|a| matches!(a, wirelab_core::program::Action::CompAction { action, .. } if action == "on")),
        "LED lights after the delay: {lit:?}"
    );
    let done = host.on_press(btn);
    assert!(
        done.iter().any(|a| matches!(a, wirelab_core::program::Action::CompAction { action, .. } if action == "beep")),
        "reaction press beeps: {done:?}"
    );
    assert!(host.take_logs().iter().any(|l| l.contains("reaction:")));
}

#[test]
fn pwm_breathe_example_emits_pwm() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/02-pwm-breathe.wirelab.json"),
    )
    .expect("example parses");
    let board = lib.board(&project.circuit.board_id).expect("board");
    let dot = *project.circuit.components.keys().next().expect("controller dot");

    let mut host = wirelab_core::script::ScriptHost::new();
    host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
    let fresh = host.sync(&project.circuit, &lib);
    assert_eq!(fresh, vec![dot], "script compiled: {:?}", host.errors);
    host.on_start(dot);
    let actions = host.tick(500);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            wirelab_core::program::Action::SetPwm { gpio: 2, freq_hz: 1000, .. }
        )),
        "expected PWM on GPIO2, got {actions:?}"
    );
}

#[test]
fn rgb_rainbow_example_drives_the_led_and_boot_button() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/01-rgb-rainbow.wirelab.json"),
    )
    .expect("example parses");
    let board = lib.board(&project.circuit.board_id).expect("board");
    let dot = *project.circuit.components.keys().next().expect("controller dot");

    let mut host = wirelab_core::script::ScriptHost::new();
    host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
    host.sync(&project.circuit, &lib);
    let setup = host.on_start(dot);
    assert!(
        setup.iter().any(|a| matches!(
            a,
            wirelab_core::program::Action::SetPinMode {
                gpio: 28,
                mode: wirelab_proto::PinMode::InputPullUp
            }
        )),
        "BOOT pin configured: {setup:?}"
    );
    let actions = host.tick(100);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, wirelab_core::program::Action::SetRgb { gpio: 27, .. })),
        "expected SetRgb on GPIO27, got {actions:?}"
    );
    // BOOT press flows through on_pin.
    let press = host.on_pin(dot, 28, false);
    let logs = host.take_logs();
    assert!(
        press.is_empty() && logs.iter().any(|l| l.contains("speed")),
        "boot press handled: {logs:?}"
    );
}

/// The morse beacon must actually emit RGB steps when ticked.
#[test]
fn morse_beacon_example_beacons() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/07-morse-beacon.wirelab.json"),
    )
    .expect("example parses");
    let board = lib.board(&project.circuit.board_id).expect("board");
    let dot = *project.circuit.components.keys().next().expect("beacon dot");

    let mut host = wirelab_core::script::ScriptHost::new();
    host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
    host.sync(&project.circuit, &lib);
    assert!(host.errors.is_empty(), "{:?}", host.errors);
    host.on_start(dot);
    let mut rgb_steps = 0;
    for t in 1..40 {
        let acts = host.tick(t * 100);
        rgb_steps += acts
            .iter()
            .filter(|a| matches!(a, wirelab_core::program::Action::SetRgb { .. }))
            .count();
        assert!(host.errors.is_empty(), "tick {t}: {:?}", host.errors);
    }
    assert!(rgb_steps > 5, "beacon produced {rgb_steps} RGB steps");
    // BOOT press switches the message without breaking anything.
    host.on_pin(dot, 28, false);
    assert!(host.take_logs().iter().any(|l| l.contains("message:")));
}

/// The LCD clock example initializes the display and paints once a second.
#[test]
fn lcd_clock_example_paints() {
    let assets = assets_dir();
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("assets parse");
    let project = wirelab_core::project::Project::load(
        &assets.join("examples/09-lcd-clock.wirelab.json"),
    )
    .expect("example parses");
    let board = lib.board(&project.circuit.board_id).expect("board");
    let dot = *project
        .circuit
        .components
        .values()
        .find(|c| c.label == "clock")
        .map(|c| &c.id)
        .expect("clock dot");

    let mut host = wirelab_core::script::ScriptHost::new();
    host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
    host.sync(&project.circuit, &lib);
    assert!(host.errors.is_empty(), "{:?}", host.errors);
    let setup = host.on_start(dot);
    assert!(setup.iter().any(|a| matches!(a, wirelab_core::program::Action::LcdInit { .. })));
    assert!(setup.iter().any(|a| matches!(a, wirelab_core::program::Action::LcdText { .. })));
    host.set_world(wirelab_core::script::World { now_ms: 2000, ..Default::default() });
    let tick = host.tick(2000);
    assert!(
        tick.iter().any(|a| matches!(
            a,
            wirelab_core::program::Action::LcdText { text, .. } if text.contains("uptime")
        )),
        "clock repaints: {tick:?}"
    );

    // Engine turns the actions into RGB565 protocol frames.
    let mut engine = wirelab_core::engine::Engine::default();
    let msgs = engine.run_script_actions(setup, 0);
    assert!(msgs.iter().any(|m| matches!(m, wirelab_proto::HostMsg::LcdInit { sck: 6, .. })));
    assert!(
        msgs.iter().any(|m| matches!(m, wirelab_proto::HostMsg::LcdClear { rgb565 } if *rgb565 == wirelab_core::program::rgb565([0, 0, 40])))
    );
}
