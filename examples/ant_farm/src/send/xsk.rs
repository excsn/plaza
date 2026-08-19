//! The AF_XDP send arm: TX only, by design.
//!
//! Transmit needs no BPF redirect program, so this stays a socket and a ring:
//! frames go out through the XSK, and everything inbound (ops, probe pongs)
//! keeps arriving at the plain UDP socket, because the frames built here name
//! that socket's address and port as their source. One process, two exits,
//! one entrance.
//!
//! Requirements this cannot paper over: CAP_NET_ADMIN (or root), a Linux
//! kernel new enough for AF_XDP (5.x), and a destination MAC for the next
//! hop, since bypassing the kernel stack also bypasses its neighbour table.

use std::ffi::CString;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::net::UdpSocket;
use xdpilone::xdp::XdpDesc;
use xdpilone::{BufIdx, DeviceQueue, IfInfo, RingTx, Socket, SocketConfig, Umem, UmemConfig};

use super::frame::{self, Lane};
use super::SendPath;
use crate::panel::WireStats;

const FRAME_SIZE: u32 = 1 << 12;
const FRAME_COUNT: u32 = 1 << 11;
const TX_RING: u32 = 1 << 11;

struct Rings {
  umem: Umem,
  tx: RingTx,
  device: DeviceQueue,
  free: Vec<u32>,
}

// The umem area is leaked for the life of the process and every ring and
// frame access happens under the one mutex around `Rings`.
unsafe impl Send for Rings {}

pub struct XdpSend {
  lane: Lane,
  rings: Mutex<Rings>,
  wire: Arc<WireStats>,
}

impl XdpSend {
  pub fn open(iface: &str, args: &[String], socket: &UdpSocket, wire: Arc<WireStats>) -> Result<Self, String> {
    let local = socket.local_addr().map_err(|e| e.to_string())?;
    let src_port = local.port();
    let src_ip = match arg_str(args, "--xdp-src-ip") {
      Some(ip) => ip.parse::<std::net::Ipv4Addr>().map_err(|e| e.to_string())?.octets(),
      None => match local {
        SocketAddr::V4(v4) if !v4.ip().is_unspecified() => v4.ip().octets(),
        _ => return Err("bound to a wildcard address; pass --xdp-src-ip".into()),
      },
    };
    let src_mac = match arg_str(args, "--xdp-src-mac") {
      Some(mac) => frame::parse_mac(&mac).ok_or("--xdp-src-mac wants aa:bb:cc:dd:ee:ff")?,
      None => sysfs_mac(iface)?,
    };
    let dst_mac = frame::parse_mac(&arg_str(args, "--xdp-dst-mac").ok_or(
      "--xdp-dst-mac is required: the next hop's MAC, because kernel bypass also bypasses ARP",
    )?)
    .ok_or("--xdp-dst-mac wants aa:bb:cc:dd:ee:ff")?;
    let queue: u32 = arg_str(args, "--xdp-queue").and_then(|q| q.parse().ok()).unwrap_or(0);

    let lane = Lane {
      src_mac,
      dst_mac,
      src_ip,
      src_port,
    };

    let bytes = (FRAME_SIZE * FRAME_COUNT) as usize;
    let layout = std::alloc::Layout::from_size_align(bytes, 4096).map_err(|e| e.to_string())?;
    let area = unsafe { std::alloc::alloc_zeroed(layout) };
    let area = NonNull::new(std::ptr::slice_from_raw_parts_mut(area, bytes)).ok_or("umem allocation failed")?;

    let config = UmemConfig {
      frame_size: FRAME_SIZE,
      ..UmemConfig::default()
    };
    let umem = unsafe { Umem::new(config, area) }.map_err(|e| format!("umem: {e:?}"))?;

    let name = CString::new(iface).map_err(|e| e.to_string())?;
    let mut info = IfInfo::invalid();
    info.from_name(&name).map_err(|e| format!("interface {iface}: {e:?}"))?;
    info.set_queue(queue);

    let sock = Socket::with_shared(&info, &umem).map_err(|e| format!("socket: {e:?}"))?;
    let device = umem.fq_cq(&sock).map_err(|e| format!("fq/cq: {e:?}"))?;
    let rxtx = umem
      .rx_tx(
        &sock,
        &SocketConfig {
          rx_size: None,
          tx_size: NonZeroU32::new(TX_RING),
          bind_flags: SocketConfig::XDP_BIND_NEED_WAKEUP,
        },
      )
      .map_err(|e| format!("rx/tx: {e:?}"))?;
    let tx = rxtx.map_tx().map_err(|e| format!("tx map: {e:?}"))?;
    umem.bind(&rxtx).map_err(|e| format!("bind (CAP_NET_ADMIN?): {e:?}"))?;

    tracing::info!(iface, queue, "AF_XDP send arm up");
    Ok(Self {
      lane,
      rings: Mutex::new(Rings {
        umem,
        tx,
        device,
        free: (0..FRAME_COUNT).rev().collect(),
      }),
      wire,
    })
  }
}

impl SendPath for XdpSend {
  fn send(&self, to: SocketAddr, bytes: &[u8]) {
    let SocketAddr::V4(to) = to else {
      self.wire.dropped.fetch_add(1, Ordering::Relaxed);
      return;
    };
    if frame::HEADER + bytes.len() > FRAME_SIZE as usize {
      self.wire.dropped.fetch_add(1, Ordering::Relaxed);
      return;
    }

    let begun = Instant::now();
    let mut guard = self.rings.lock();
    let rings = &mut *guard;

    {
      let mut reader = rings.device.complete(FRAME_COUNT);
      while let Some(addr) = reader.read() {
        rings.free.push((addr / u64::from(FRAME_SIZE)) as u32);
      }
      reader.release();
    }

    let Some(idx) = rings.free.pop() else {
      self.wire.dropped.fetch_add(1, Ordering::Relaxed);
      return;
    };
    let Some(mut chunk) = rings.umem.frame(BufIdx(idx)) else {
      self.wire.dropped.fetch_add(1, Ordering::Relaxed);
      return;
    };

    let len = frame::write_udp_frame(
      unsafe { chunk.addr.as_mut() },
      &self.lane,
      to.ip().octets(),
      to.port(),
      bytes,
    );
    let desc = XdpDesc {
      addr: chunk.offset,
      len: len as u32,
      options: 0,
    };

    {
      let mut writer = rings.tx.transmit(1);
      if writer.insert_once(desc) {
        writer.commit();
      } else {
        drop(writer);
        rings.free.push(idx);
        self.wire.dropped.fetch_add(1, Ordering::Relaxed);
        return;
      }
    }
    if rings.tx.needs_wakeup() {
      rings.tx.wake();
    }
    drop(guard);

    self.wire.record(bytes.len(), begun.elapsed().as_nanos() as u64);
  }

  fn label(&self) -> &'static str {
    "xdp"
  }
}

fn arg_str(args: &[String], flag: &str) -> Option<String> {
  args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn sysfs_mac(iface: &str) -> Result<[u8; 6], String> {
  let path = format!("/sys/class/net/{iface}/address");
  let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
  frame::parse_mac(text.trim()).ok_or_else(|| format!("{path} held no MAC"))
}
