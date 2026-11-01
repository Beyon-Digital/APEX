//! KiteTicker binary packet decoding.
//!
//! A WebSocket binary frame is `[u16 packet_count]` then, per packet,
//! `[u16 length][packet bytes]`. All integers are big-endian. Packet length
//! selects the mode: 8 = LTP, 28/32 = index quote/full, 44 = quote, 184 = full.

use super::instruments::InstrumentMaster;

#[derive(Debug, Clone, PartialEq)]
pub struct TickerQuote {
    pub token: u32,
    pub last: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<u64>,
    pub exchange_ts: Option<i64>,
}

impl TickerQuote {
    /// Percent change versus the previous close, when known.
    pub fn change_pct(&self) -> Option<f64> {
        let close = self.close.filter(|c| *c > 0.0)?;
        Some((self.last - close) / close * 100.0)
    }
}

fn be_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be_i32(data: &[u8], offset: usize) -> Option<i32> {
    be_u32(data, offset).map(|v| v as i32)
}

/// Decode one binary frame. Unknown packet lengths are skipped. Heartbeats
/// (frames of one byte) decode to an empty list.
pub fn decode_frame(frame: &[u8], instruments: &InstrumentMaster) -> Vec<TickerQuote> {
    let mut out = Vec::new();
    if frame.len() < 2 {
        return out;
    }
    let count = u16::from_be_bytes([frame[0], frame[1]]) as usize;
    let mut offset = 2;
    for _ in 0..count {
        let Some(len_bytes) = frame.get(offset..offset + 2) else { break };
        let len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
        offset += 2;
        let Some(packet) = frame.get(offset..offset + len) else { break };
        offset += len;
        if let Some(quote) = decode_packet(packet, instruments) {
            out.push(quote);
        }
    }
    out
}

fn decode_packet(p: &[u8], instruments: &InstrumentMaster) -> Option<TickerQuote> {
    let token = be_u32(p, 0)?;
    let divisor = instruments
        .by_token(token)
        .map(|i| i.price_divisor())
        .unwrap_or(match token & 0xff {
            3 => 10_000_000.0,
            6 => 10_000.0,
            _ => 100.0,
        });
    let price = |offset: usize| be_i32(p, offset).map(|v| v as f64 / divisor);

    match p.len() {
        8 => Some(TickerQuote {
            token,
            last: price(4)?,
            bid: None,
            ask: None,
            open: None,
            high: None,
            low: None,
            close: None,
            volume: None,
            exchange_ts: None,
        }),
        // Index quote (28) and full (32): ltp, high, low, open, close, change
        28 | 32 => Some(TickerQuote {
            token,
            last: price(4)?,
            bid: None,
            ask: None,
            high: price(8),
            low: price(12),
            open: price(16),
            close: price(20),
            volume: None,
            exchange_ts: if p.len() == 32 { be_u32(p, 28).map(i64::from) } else { None },
        }),
        // Quote (44) and full (184): ltp, last_qty, avg, volume, buy_q, sell_q, o, h, l, c
        44 | 184 => {
            let mut quote = TickerQuote {
                token,
                last: price(4)?,
                bid: None,
                ask: None,
                open: price(28),
                high: price(32),
                low: price(36),
                close: price(40),
                volume: be_u32(p, 16).map(u64::from),
                exchange_ts: None,
            };
            if p.len() == 184 {
                quote.exchange_ts = be_u32(p, 60).map(i64::from);
                // Depth starts at 64: 5 buy then 5 sell entries of 12 bytes
                // (qty u32, price u32, orders u16, pad u16).
                quote.bid = price(64 + 4).filter(|v| *v > 0.0);
                quote.ask = price(64 + 5 * 12 + 4).filter(|v| *v > 0.0);
            }
            Some(quote)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn frame(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (packets.len() as u16).to_be_bytes().to_vec();
        for p in packets {
            out.extend((p.len() as u16).to_be_bytes());
            out.extend(p);
        }
        out
    }

    #[test]
    fn decodes_ltp_and_full_packets() {
        let master = InstrumentMaster::new();

        let mut ltp = vec![0u8; 8];
        put(&mut ltp, 0, 738561);
        put(&mut ltp, 4, 124_750); // 1247.50

        let mut full = vec![0u8; 184];
        put(&mut full, 0, 408065);
        put(&mut full, 4, 150_000);
        put(&mut full, 16, 12_345);
        put(&mut full, 28, 149_000);
        put(&mut full, 32, 151_000);
        put(&mut full, 36, 148_500);
        put(&mut full, 40, 148_000);
        put(&mut full, 68, 149_950); // best bid
        put(&mut full, 64 + 60 + 4, 150_050); // best ask

        let quotes = decode_frame(&frame(&[ltp, full]), &master);
        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[0].token, 738561);
        assert!((quotes[0].last - 1247.5).abs() < 1e-9);
        assert_eq!(quotes[1].volume, Some(12_345));
        assert!((quotes[1].bid.unwrap() - 1499.5).abs() < 1e-9);
        assert!((quotes[1].ask.unwrap() - 1500.5).abs() < 1e-9);
        let pct = quotes[1].change_pct().unwrap();
        assert!((pct - (1500.0 - 1480.0) / 1480.0 * 100.0).abs() < 1e-9);
    }

    #[test]
    fn heartbeat_and_truncated_frames_are_safe() {
        let master = InstrumentMaster::new();
        assert!(decode_frame(&[0u8], &master).is_empty());
        // Claims one 44-byte packet but is cut short.
        assert!(decode_frame(&[0, 1, 0, 44, 0, 0], &master).is_empty());
    }

    #[test]
    fn currency_segment_uses_larger_divisor() {
        let master = InstrumentMaster::new();
        let mut ltp = vec![0u8; 8];
        put(&mut ltp, 0, 0x0000_0003); // segment 3 = CDS
        put(&mut ltp, 4, 830_000_000);
        let quotes = decode_frame(&frame(&[ltp]), &master);
        assert!((quotes[0].last - 83.0).abs() < 1e-9);
    }
}
