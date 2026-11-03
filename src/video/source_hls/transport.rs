//! Repair one malformed source-TS padding pattern before demuxing. No decoding.
use gst::prelude::*;
use std::{collections::BTreeMap, sync::Mutex};
const SIZE: usize = 188;
#[derive(Default)]
struct Counter {
    last: Option<u8>,
    removed: u8,
}
#[derive(Default)]
struct Repair {
    tail: Vec<u8>,
    counters: BTreeMap<u16, Counter>,
    checked: bool,
    bypass: bool,
    repaired: u64,
    discont: bool,
    metadata: Option<gst::Buffer>,
}
impl Repair {
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.tail);
        out.extend_from_slice(bytes);
        if !self.checked {
            if out.len() < SIZE * 2 {
                self.tail = out;
                return Vec::new();
            }
            self.checked = true;
            self.bypass = out[0] != 0x47 || out[SIZE] != 0x47;
        }
        // Other packet sizes/formats pass through unchanged.
        if self.bypass {
            return out;
        }
        let complete = out.len() / SIZE * SIZE;
        if out[..complete].chunks_exact(SIZE).any(|p| p[0] != 0x47) {
            self.bypass = true;
            return out;
        }
        self.tail = out.split_off(complete);
        for p in out.chunks_exact_mut(SIZE) {
            let pid = (u16::from(p[1] & 31) << 8) | u16::from(p[2]);
            if pid == 0x1fff {
                continue;
            }
            let cc = p[3] & 15;
            let afc = (p[3] >> 4) & 3;
            let counter = self.counters.entry(pid).or_default();
            // Payload flag set, but the entire packet is empty adaptation
            // stuffing. No PCR/discontinuity/scrambling/PES data may be lost.
            if afc == 3
                && p[1] & 0xc0 == 0
                && p[3] & 0xc0 == 0
                && p[4] == 183
                && p[5] == 0
                && p[6..].iter().all(|b| *b == 0xff)
            {
                if counter.last != Some(cc) {
                    counter.removed = (counter.removed + 1) & 15;
                }
                counter.last = Some(cc);
                p.fill(0xff);
                p[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
                self.repaired += 1;
            } else {
                // Subtract ONLY the removed fake increments. Real missing or
                // duplicated packets keep their discontinuities for demuxing.
                p[3] = (p[3] & 0xf0) | cc.wrapping_sub(counter.removed) & 15;
                if afc & 1 != 0 {
                    counter.last = Some(cc);
                }
            }
        }
        out
    }
}
impl Repair {
    fn buffer(
        &mut self,
        buffer: &gst::BufferRef,
    ) -> Result<Option<gst::Buffer>, gst::glib::BoolError> {
        if buffer.flags().contains(gst::BufferFlags::DISCONT) {
            // A buffer discontinuity need not flush the demuxer. Keep the
            // counter correction bias; discard only incomplete old bytes.
            // A real seek's FlushStop resets all state in the event probe.
            self.tail.clear();
            self.metadata = None;
            self.checked = false;
            self.bypass = false;
            self.discont = true;
        }
        if self.bypass {
            return Ok(Some(buffer.to_owned()));
        }
        let metadata = |b: &gst::BufferRef| -> Result<gst::Buffer, gst::glib::BoolError> {
            let mut copy = gst::Buffer::new();
            b.copy_into(copy.make_mut(), gst::BUFFER_COPY_METADATA, ..)?;
            Ok(copy)
        };
        if self.metadata.is_none() {
            self.metadata = Some(metadata(buffer)?);
        }
        let map = buffer.map_readable()?;
        let previous = self.repaired;
        let data = self.feed(map.as_slice());
        if previous == 0 && self.repaired > 0 {
            crate::diagnostics::record(
                "source_ts_padding_repaired",
                serde_json::json!({
                "packets":self.repaired,"payload_preserved":true,"real_loss_detection_preserved":true}),
            );
        }
        if data.is_empty() {
            return Ok(None);
        }
        let mut next = gst::Buffer::from_mut_slice(data);
        let first = self.metadata.take().unwrap();
        first.copy_into(next.make_mut(), gst::BUFFER_COPY_METADATA, ..)?;
        if first.offset() != u64::MAX {
            let end = first.offset().saturating_add(next.size() as u64);
            next.make_mut().set_offset_end(end);
        }
        if !self.tail.is_empty() {
            let mut pending = metadata(buffer)?;
            pending.make_mut().unset_flags(gst::BufferFlags::DISCONT);
            if buffer.offset() != u64::MAX {
                pending.make_mut().set_offset(
                    buffer
                        .offset()
                        .saturating_add((buffer.size() - self.tail.len()) as u64),
                );
            }
            self.metadata = Some(pending);
        }
        if self.discont {
            next.make_mut().set_flags(gst::BufferFlags::DISCONT);
            self.discont = false;
        }
        Ok(Some(next))
    }
}
pub(super) fn install(element: &gst::Element) {
    if element.factory().is_none_or(|f| f.name() != "tsdemux") {
        return;
    }
    let Some(pad) = element.static_pad("sink") else {
        return;
    };
    let weak = element.downgrade();
    let state = Mutex::new(Repair::default());
    pad.add_probe(
        gst::PadProbeType::BUFFER
            | gst::PadProbeType::BUFFER_LIST
            | gst::PadProbeType::EVENT_DOWNSTREAM
            | gst::PadProbeType::EVENT_FLUSH,
        move |_, info| {
            let mut s = state.lock().unwrap();
            if let Some(event) = info.event() {
                match event.view() {
                    gst::EventView::FlushStop(_) => *s = Repair::default(),
                    gst::EventView::Eos(_) if s.repaired > 0 => crate::diagnostics::record(
                        "source_ts_padding_summary",
                        serde_json::json!({"packets":s.repaired,"partial_bytes":s.tail.len()}),
                    ),
                    _ => {}
                }
                return gst::PadProbeReturn::Ok;
            }
            let result: Result<Option<gst::PadProbeData>, gst::glib::BoolError> =
                (|| match info.data.as_ref() {
                    Some(gst::PadProbeData::Buffer(b)) => {
                        Ok(s.buffer(b)?.map(gst::PadProbeData::Buffer))
                    }
                    Some(gst::PadProbeData::BufferList(list)) => {
                        let mut output = gst::BufferList::new();
                        for buffer in list.iter() {
                            if let Some(buffer) = s.buffer(buffer)? {
                                output.make_mut().add(buffer);
                            }
                        }
                        Ok((!output.is_empty()).then_some(gst::PadProbeData::BufferList(output)))
                    }
                    _ => return Ok(None),
                })();
            match result {
                Ok(Some(data)) => {
                    info.data = Some(data);
                    gst::PadProbeReturn::Ok
                }
                Ok(None) => gst::PadProbeReturn::Drop,
                Err(_) => {
                    if let Some(element) = weak.upgrade() {
                        gst::element_error!(
                            element,
                            gst::StreamError::Failed,
                            ("Unable to preserve source transport data")
                        );
                    }
                    gst::PadProbeReturn::Drop
                }
            }
        },
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    fn payload(cc: u8) -> Vec<u8> {
        let mut p = vec![0x55; SIZE];
        p[..4].copy_from_slice(&[0x47, 1, 0, 0x10 | cc]);
        p
    }
    fn padding(cc: u8) -> Vec<u8> {
        let mut p = vec![0xff; SIZE];
        p[..6].copy_from_slice(&[0x47, 1, 0, 0x30 | cc, 183, 0]);
        p
    }
    #[test]
    fn split_buffers_preserve_first_timestamp_and_discontinuity() {
        gst::init().unwrap();
        let data = [payload(0), padding(1), payload(2)].concat();
        let mut first = gst::Buffer::from_mut_slice(data[..17].to_vec());
        first.make_mut().set_pts(gst::ClockTime::from_seconds(3));
        first.make_mut().set_flags(gst::BufferFlags::DISCONT);
        let mut s = Repair::default();
        assert!(s.buffer(&first).unwrap().is_none());
        let second = gst::Buffer::from_mut_slice(data[17..].to_vec());
        let out = s.buffer(&second).unwrap().unwrap();
        assert_eq!(out.pts(), first.pts());
        assert!(out.flags().contains(gst::BufferFlags::DISCONT));
        assert_eq!(out.size(), data.len());
    }
    #[test]
    fn buffer_discontinuity_does_not_reintroduce_fake_counter_gaps() {
        gst::init().unwrap();
        let mut s = Repair::default();
        let first = gst::Buffer::from_mut_slice([payload(0), padding(1), padding(2)].concat());
        s.buffer(&first).unwrap().unwrap();
        let mut next = gst::Buffer::from_mut_slice([payload(3), payload(4)].concat());
        next.make_mut().set_flags(gst::BufferFlags::DISCONT);
        let output = s.buffer(&next).unwrap().unwrap();
        let data = output.map_readable().unwrap();
        assert_eq!(data[3] & 15, 1);
        assert_eq!(data[SIZE + 3] & 15, 2);
        assert!(output.flags().contains(gst::BufferFlags::DISCONT));
    }
    #[test]
    fn removes_fake_increments_without_changing_pes_bytes() {
        let input = [payload(0), padding(1), padding(2), payload(3), payload(4)].concat();
        let mut s = Repair::default();
        let out = s.feed(&input);
        assert_eq!(s.repaired, 2);
        assert_eq!(out.len(), input.len());
        assert_eq!(&out[..SIZE], &input[..SIZE]);
        for (i, cc) in [(3, 1), (4, 2)] {
            assert_eq!(out[i * SIZE + 3] & 15, cc);
            assert_eq!(
                &out[i * SIZE + 4..(i + 1) * SIZE],
                &input[i * SIZE + 4..(i + 1) * SIZE]
            );
        }
        assert_eq!(&out[SIZE..SIZE + 4], &[0x47, 0x1f, 0xff, 0x10]);
    }
    #[test]
    fn genuine_loss_and_duplicates_are_not_concealed() {
        let mut s = Repair::default();
        let out = s.feed(&[payload(0), padding(1), payload(5), payload(5)].concat());
        assert_eq!(out[2 * SIZE + 3] & 15, 4);
        assert_eq!(out[3 * SIZE + 3] & 15, 4);
    }
    #[test]
    fn fragmented_network_reads_and_bounded_tail() {
        let input = [payload(14), padding(15), payload(0), payload(1)].concat();
        let expected = Repair::default().feed(&input);
        for width in [1, 3, 187, 188, 189, 375, 4096] {
            let mut s = Repair::default();
            let mut out = Vec::new();
            for chunk in input.chunks(width) {
                out.extend(s.feed(chunk));
                assert!(s.tail.len() < SIZE * 2);
            }
            assert_eq!(out, expected);
            assert!(s.tail.is_empty());
        }
    }
    #[test]
    fn valid_padding_pcr_and_other_packet_sizes_are_untouched() {
        let mut valid = padding(0);
        valid[3] = 0x20; // Valid adaptation-only stuffing.
        let mut pcr = padding(1);
        pcr[5] = 0x10;
        let input = [payload(0), valid, pcr, payload(2)].concat();
        assert_eq!(Repair::default().feed(&input), input);
        let m2ts = vec![0x22; 192 * 4];
        assert_eq!(Repair::default().feed(&m2ts), m2ts);
    }
    #[test]
    fn independent_pids_and_reset() {
        let mut audio = payload(7);
        audio[2] = 1;
        let mut s = Repair::default();
        let out = s.feed(&[payload(0), padding(1), audio].concat());
        assert_eq!(out[2 * SIZE + 3] & 15, 7);
        s = Repair::default();
        let input = [payload(9), payload(10)].concat();
        assert_eq!(s.feed(&input), input);
    }
}
