//! Ethernet, IPv4 and UDP headers, built by hand.
//!
//! AF_XDP hands the NIC raw frames, so everything the kernel stack normally
//! writes is this module's to write. Pure and platform-independent on
//! purpose: the arithmetic is testable on any machine, only the socket that
//! consumes these frames is Linux's.

/// Ethernet (14) + IPv4 without options (20) + UDP (8).
pub const HEADER: usize = 42;

/// The fixed half of every frame this process sends: one NIC, one source.
/// The destination MAC is the next hop (the gateway, for anything routed),
/// never the final recipient's.
#[derive(Clone, Copy, Debug)]
pub struct Lane {
  pub src_mac: [u8; 6],
  pub dst_mac: [u8; 6],
  pub src_ip: [u8; 4],
  pub src_port: u16,
}

/// Writes one UDP frame into `buf`, returning its total length.
///
/// The UDP checksum is left zero, which IPv4 permits; the IPv4 header
/// checksum is computed. `buf` must hold [`HEADER`] plus the payload.
pub fn write_udp_frame(buf: &mut [u8], lane: &Lane, dst_ip: [u8; 4], dst_port: u16, payload: &[u8]) -> usize {
  let total = HEADER + payload.len();
  assert!(buf.len() >= total, "frame buffer too small");

  buf[0..6].copy_from_slice(&lane.dst_mac);
  buf[6..12].copy_from_slice(&lane.src_mac);
  buf[12..14].copy_from_slice(&0x0800u16.to_be_bytes());

  let ip = &mut buf[14..34];
  ip[0] = 0x45;
  ip[1] = 0;
  ip[2..4].copy_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
  ip[4..6].copy_from_slice(&[0, 0]);
  ip[6..8].copy_from_slice(&[0x40, 0]);
  ip[8] = 64;
  ip[9] = 17;
  ip[10..12].copy_from_slice(&[0, 0]);
  ip[12..16].copy_from_slice(&lane.src_ip);
  ip[16..20].copy_from_slice(&dst_ip);
  let checksum = ipv4_checksum(ip);
  buf[24..26].copy_from_slice(&checksum.to_be_bytes());

  let udp = &mut buf[34..42];
  udp[0..2].copy_from_slice(&lane.src_port.to_be_bytes());
  udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
  udp[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
  udp[6..8].copy_from_slice(&[0, 0]);

  buf[42..total].copy_from_slice(payload);
  total
}

fn ipv4_checksum(header: &[u8]) -> u16 {
  let mut sum = 0u32;
  for pair in header.chunks_exact(2) {
    sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
  }
  while sum > 0xffff {
    sum = (sum & 0xffff) + (sum >> 16);
  }
  !(sum as u16)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lane() -> Lane {
    Lane {
      src_mac: [0x02, 0, 0, 0, 0, 0x01],
      dst_mac: [0x02, 0, 0, 0, 0, 0x02],
      src_ip: [192, 168, 1, 10],
      src_port: 4747,
    }
  }

  #[test]
  fn the_ip_header_checksums_to_all_ones_with_its_own_checksum_in_place() {
    let mut buf = [0u8; 128];
    let len = write_udp_frame(&mut buf, &lane(), [192, 168, 1, 20], 5555, b"hello ants");
    assert_eq!(len, HEADER + 10);

    let mut sum = 0u32;
    for pair in buf[14..34].chunks_exact(2) {
      sum += u32::from(u16::from_be_bytes([pair[0], pair[1]]));
    }
    while sum > 0xffff {
      sum = (sum & 0xffff) + (sum >> 16);
    }
    assert_eq!(sum, 0xffff, "a valid IPv4 header sums to ones-complement zero");
  }

  #[test]
  fn every_field_lands_where_a_capture_would_expect_it() {
    let mut buf = [0u8; 128];
    let payload = [7u8; 32];
    let len = write_udp_frame(&mut buf, &lane(), [10, 0, 0, 9], 40000, &payload);

    assert_eq!(&buf[0..6], &[0x02, 0, 0, 0, 0, 0x02], "destination MAC first on the wire");
    assert_eq!(&buf[12..14], &[0x08, 0x00], "ethertype IPv4");
    assert_eq!(buf[14], 0x45);
    assert_eq!(u16::from_be_bytes([buf[16], buf[17]]) as usize, 20 + 8 + payload.len());
    assert_eq!(buf[23], 17, "protocol UDP");
    assert_eq!(&buf[30..34], &[10, 0, 0, 9]);
    assert_eq!(u16::from_be_bytes([buf[34], buf[35]]), 4747);
    assert_eq!(u16::from_be_bytes([buf[36], buf[37]]), 40000);
    assert_eq!(u16::from_be_bytes([buf[38], buf[39]]) as usize, 8 + payload.len());
    assert_eq!(&buf[42..len], &payload);
  }
}
