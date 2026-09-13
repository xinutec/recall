use audiod::meter::StreamMeter;

fn pcm(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[test]
fn peak_and_totals_accumulate() {
    let mut meter = StreamMeter::new(48_000, 1);
    assert_eq!(meter.feed(&pcm(&[0, 5, -300])), 300);
    assert_eq!(meter.feed(&pcm(&[100])), 100);
    assert_eq!(meter.peak, 300);
    assert_eq!(meter.bytes_total, 8);
}

#[test]
fn a_half_sample_carries_to_the_next_feed() {
    let mut meter = StreamMeter::new(48_000, 1);
    let bytes = pcm(&[0, 0, 1000]); // audible sample is the third
    meter.feed(&bytes[..3]); // splits the second sample
    meter.feed(&bytes[3..]);
    assert_eq!(meter.peak, 1000);
    // Offset counts from the true stream start despite the split read.
    assert_eq!(meter.first_audible_byte, Some(4));
}

#[test]
fn digital_silence_has_no_peak_db_and_no_audible_time() {
    let mut meter = StreamMeter::new(48_000, 1);
    meter.feed(&pcm(&[0, 0, 0]));
    assert_eq!(meter.peak_db(), None);
    assert_eq!(meter.first_audible_s(), None);
}

#[test]
fn full_scale_is_zero_dbfs() {
    let mut meter = StreamMeter::new(48_000, 1);
    meter.feed(&pcm(&[i16::MIN])); // |-32768| = full scale
    assert_eq!(meter.peak_db(), Some(0.0));
}

#[test]
fn first_audible_seconds_use_the_stream_clock() {
    let mut meter = StreamMeter::new(2, 1); // 4 bytes/s for easy arithmetic
    meter.feed(&pcm(&[0, 0, 0, 0, 500]));
    assert_eq!(meter.first_audible_s(), Some(2.0));
}
