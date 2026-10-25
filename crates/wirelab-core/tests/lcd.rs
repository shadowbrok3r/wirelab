//! Simulated ST7735 op log: init, clears, covering fills and the op cap.

use wirelab_core::sim::{LcdOp, apply_lcd_msg};
use wirelab_proto::HostMsg;

fn text(x: u8, y: u8, s: &str) -> HostMsg {
    HostMsg::LcdText {
        x,
        y,
        rgb565: 0xFFFF,
        text: s.try_into().unwrap(),
    }
}

fn rect(x: u8, y: u8, w: u8, h: u8, rgb565: u16) -> HostMsg {
    HostMsg::LcdRect { x, y, w, h, rgb565 }
}

fn init() -> HostMsg {
    HostMsg::LcdInit {
        sck: 6,
        mosi: 7,
        cs: 8,
        dc: 9,
        rst: 10,
        bl: 255,
    }
}

#[test]
fn draws_before_init_are_ignored() {
    let mut lcd = None;
    assert!(!apply_lcd_msg(&mut lcd, &text(0, 0, "hi")));
    assert!(lcd.is_none());
    assert!(apply_lcd_msg(&mut lcd, &init()));
    assert_eq!(lcd, Some(vec![LcdOp::Clear([0, 0, 0])]));
}

#[test]
fn a_fill_drops_the_text_it_covers_but_keeps_the_base_clear() {
    let mut lcd = None;
    apply_lcd_msg(&mut lcd, &init());
    apply_lcd_msg(&mut lcd, &text(4, 4, "READY"));
    apply_lcd_msg(&mut lcd, &text(4, 40, "keep"));
    apply_lcd_msg(&mut lcd, &rect(0, 0, 128, 20, 0));
    let ops = lcd.unwrap();
    assert_eq!(ops.len(), 3, "{ops:?}");
    assert!(matches!(ops[0], LcdOp::Clear(_)));
    assert!(matches!(&ops[1], LcdOp::Text { text, .. } if text == "keep"));
    assert!(matches!(ops[2], LcdOp::Rect { .. }));
}

#[test]
fn a_full_panel_fill_becomes_a_clear() {
    let mut lcd = None;
    apply_lcd_msg(&mut lcd, &init());
    apply_lcd_msg(&mut lcd, &text(4, 4, "x"));
    apply_lcd_msg(&mut lcd, &rect(0, 0, 128, 128, 0xF800));
    assert_eq!(lcd, Some(vec![LcdOp::Clear([248, 0, 0])]));
}

#[test]
fn a_redraw_loop_stays_bounded_and_keeps_its_background() {
    let mut lcd = None;
    apply_lcd_msg(&mut lcd, &init());
    apply_lcd_msg(&mut lcd, &rect(0, 0, 128, 128, 0x001F));
    for i in 0..5000u32 {
        apply_lcd_msg(&mut lcd, &rect(0, 60, 128, 12, 0x001F));
        apply_lcd_msg(&mut lcd, &text(2, 61, &format!("t={i}")));
    }
    let ops = lcd.unwrap();
    assert!(ops.len() <= 3, "covered ops pile up: {}", ops.len());
    assert_eq!(ops[0], LcdOp::Clear([0, 0, 248]));
}

#[test]
fn uncovered_text_is_capped_without_losing_the_base_clear() {
    let mut lcd = None;
    apply_lcd_msg(&mut lcd, &init());
    for i in 0..3000u32 {
        apply_lcd_msg(&mut lcd, &text((i % 100) as u8, 0, "."));
    }
    let ops = lcd.unwrap();
    assert!(ops.len() <= 1024);
    assert!(matches!(ops[0], LcdOp::Clear(_)));
}
