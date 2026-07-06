//! Wi-Fi station + TCP link server: the same framed protocol as UART0,
//! carried over a smoltcp TCP socket, plus UDP discovery beacons.

use core::fmt::Write as _;
use core::net::Ipv4Addr;

use esp_radio::wifi::{Config, Interface, WifiController, sta::StationConfig};
use smoltcp::iface::{Interface as SmolIface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, IpEndpoint};
use wirelab_proto::frame::Decoder;
use wirelab_proto::{DISCOVERY_PORT, HostMsg, TCP_LINK_PORT, WifiState};

const BEACON_PERIOD_US: u64 = 2_000_000;
const CONNECT_GRACE_US: u64 = 15_000_000;
const RECONNECT_PERIOD_US: u64 = 20_000_000;
const MTU: usize = 1492;

/// Heap + preemptive scheduler; must run before any radio use.
pub fn start_scheduler(
    timg0: esp_hal::peripherals::TIMG0<'static>,
    sw: esp_hal::peripherals::SW_INTERRUPT<'static>,
) {
    esp_alloc::heap_allocator!(size: 100 * 1024);
    let timg0 = esp_hal::timer::timg::TimerGroup::new(timg0);
    let sw = esp_hal::interrupt::software::SoftwareInterruptControl::new(sw);
    esp_rtos::start(timg0.timer0, sw.software_interrupt0);
}

/// smoltcp device over esp-radio's raw rx/tx token API.
struct SmolDev<'a>(&'a mut Interface);

struct RxTok(esp_radio::wifi::WifiRxToken);
struct TxTok(esp_radio::wifi::WifiTxToken);

impl smoltcp::phy::RxToken for RxTok {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        self.0.consume_token(|buf| f(buf))
    }
}

impl smoltcp::phy::TxToken for TxTok {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        self.0.consume_token(len, f)
    }
}

impl Device for SmolDev<'_> {
    type RxToken<'b>
        = RxTok
    where
        Self: 'b;
    type TxToken<'b>
        = TxTok
    where
        Self: 'b;

    fn receive(&mut self, _t: SmolInstant) -> Option<(RxTok, TxTok)> {
        self.0.receive().map(|(r, t)| (RxTok(r), TxTok(t)))
    }

    fn transmit(&mut self, _t: SmolInstant) -> Option<TxTok> {
        self.0.transmit().map(TxTok)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(1);
        caps
    }
}

/// Poll a future just far enough to start its work, then drop it.
fn kick<F: core::future::Future>(fut: F) {
    use core::task::{Context, RawWaker, RawWakerVTable, Waker};
    fn noop_raw() -> RawWaker {
        static VTABLE: RawWakerVTable =
            RawWakerVTable::new(|_| noop_raw(), |_| {}, |_| {}, |_| {});
        RawWaker::new(core::ptr::null(), &VTABLE)
    }
    let waker = unsafe { Waker::from_raw(noop_raw()) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = fut;
    let fut = unsafe { core::pin::Pin::new_unchecked(&mut fut) };
    let _ = fut.poll(&mut cx);
}

pub struct Net {
    controller: WifiController<'static>,
    station: Interface,
    iface: SmolIface,
    sockets: SocketSet<'static>,
    tcp: SocketHandle,
    udp: SocketHandle,
    dhcp: SocketHandle,
    decoder: Decoder<HostMsg>,
    connected: bool,
    ip: [u8; 4],
    next_beacon_us: u64,
    connect_kicked_us: u64,
    last_state: WifiState,
}

impl Net {
    /// Bring the radio up as a station and start connecting; connection
    /// completion is observed later via `poll`.
    pub fn connect(ssid: &str, pass: &str, now_us: u64) -> Result<Net, ()> {
        let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
        let mut controller = WifiController::new(wifi, Default::default()).map_err(|_| ())?;
        let station_config = StationConfig::default()
            .with_ssid(ssid)
            .with_password(pass.into());
        controller
            .set_config(&Config::Station(station_config))
            .map_err(|_| ())?;
        let mut station = Interface::station();
        kick(controller.connect_async());

        let mac = station.mac_address();
        let mut cfg = smoltcp::iface::Config::new(EthernetAddress(mac).into());
        cfg.random_seed = u64::from_le_bytes([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], 0x77, 0x1b]) ^ now_us;
        let mut dev = SmolDev(&mut station);
        let iface = SmolIface::new(cfg, &mut dev, SmolInstant::from_micros(now_us as i64));

        let mut sockets = SocketSet::new(alloc::vec::Vec::new());
        let tcp = sockets.add(tcp::Socket::new(
            tcp::SocketBuffer::new(alloc::vec![0u8; 2048]),
            tcp::SocketBuffer::new(alloc::vec![0u8; 2048]),
        ));
        let udp = sockets.add(udp::Socket::new(
            udp::PacketBuffer::new(alloc::vec![udp::PacketMetadata::EMPTY; 2], alloc::vec![0u8; 128]),
            udp::PacketBuffer::new(alloc::vec![udp::PacketMetadata::EMPTY; 2], alloc::vec![0u8; 256]),
        ));
        let dhcp = sockets.add(dhcpv4::Socket::new());

        Ok(Net {
            controller,
            station,
            iface,
            sockets,
            tcp,
            udp,
            dhcp,
            decoder: Decoder::new(),
            connected: false,
            ip: [0; 4],
            next_beacon_us: 0,
            connect_kicked_us: now_us,
            last_state: WifiState::Connecting,
        })
    }

    pub fn state(&self, now_us: u64) -> WifiState {
        if self.connected {
            WifiState::Connected
        } else if now_us.wrapping_sub(self.connect_kicked_us) < CONNECT_GRACE_US {
            WifiState::Connecting
        } else {
            WifiState::Failed
        }
    }

    pub fn ip(&self) -> [u8; 4] {
        self.ip
    }

    /// Network pump: association tracking, DHCP, beacons and the TCP link.
    /// Returns decoded host messages and whether the reportable state changed.
    pub fn poll(
        &mut self,
        now_us: u64,
        chip_name: &str,
        msgs: &mut heapless::Vec<HostMsg, 8>,
    ) -> bool {
        let was_connected = self.connected;
        self.connected = self.controller.is_connected();
        if was_connected && !self.connected {
            self.ip = [0; 4];
        }
        // Re-kick the association after a drop or a failed attempt.
        if !self.connected && now_us.wrapping_sub(self.connect_kicked_us) > RECONNECT_PERIOD_US {
            kick(self.controller.connect_async());
            self.connect_kicked_us = now_us;
        }

        let ts = SmolInstant::from_micros(now_us as i64);
        let mut dev = SmolDev(&mut self.station);
        let _ = self.iface.poll(ts, &mut dev, &mut self.sockets);

        match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            Some(dhcpv4::Event::Configured(c)) => {
                self.ip = c.address.address().octets();
                self.iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    let _ = addrs.push(IpCidr::Ipv4(c.address));
                });
                if let Some(router) = c.router {
                    let _ = self.iface.routes_mut().add_default_ipv4_route(router);
                } else {
                    self.iface.routes_mut().remove_default_ipv4_route();
                }
            }
            Some(dhcpv4::Event::Deconfigured) => {
                self.ip = [0; 4];
                self.iface.update_ip_addrs(|addrs| addrs.clear());
                self.iface.routes_mut().remove_default_ipv4_route();
            }
            None => {}
        }

        if self.connected && self.ip != [0; 4] && now_us >= self.next_beacon_us {
            self.next_beacon_us = now_us + BEACON_PERIOD_US;
            let sock = self.sockets.get_mut::<udp::Socket>(self.udp);
            if !sock.is_open() {
                let _ = sock.bind(DISCOVERY_PORT);
            }
            let mut beacon = heapless::String::<80>::new();
            let ip = self.ip;
            let _ = write!(
                beacon,
                "WIRELAB1 {}.{}.{}.{} {} {}",
                ip[0], ip[1], ip[2], ip[3], TCP_LINK_PORT, chip_name
            );
            let broadcast =
                IpEndpoint::new(IpAddress::Ipv4(Ipv4Addr::BROADCAST), DISCOVERY_PORT);
            let _ = sock.send_slice(beacon.as_bytes(), broadcast);
        }

        {
            let sock = self.sockets.get_mut::<tcp::Socket>(self.tcp);
            if !sock.is_open() {
                self.decoder = Decoder::new();
                let _ = sock.listen(TCP_LINK_PORT);
            }
            if sock.state() == tcp::State::CloseWait {
                sock.close();
            }
            while sock.can_recv() {
                let decoder = &mut self.decoder;
                let _ = sock.recv(|buf| {
                    for &b in buf.iter() {
                        if let Some(Ok(msg)) = decoder.push(b) {
                            let _ = msgs.push(msg);
                        }
                    }
                    (buf.len(), ())
                });
            }
        }

        let state = self.state(now_us);
        let changed = state != self.last_state;
        self.last_state = state;
        changed
    }

    /// Queue an already-encoded frame to the TCP client, if one is attached.
    pub fn send_frame(&mut self, bytes: &[u8]) {
        let sock = self.sockets.get_mut::<tcp::Socket>(self.tcp);
        if sock.may_send() && sock.send_queue() + bytes.len() <= sock.send_capacity() {
            let _ = sock.send_slice(bytes);
        }
    }
}
