//! Thiscord's narrow, owned bindings to the bundled libopus 1.6.1 DRED API.
use super::{ffi, Decoder, Encoder, Error, Result};

impl Encoder {
    /// Redundancy duration in 10 ms units (0 disables DRED, maximum 100).
    pub fn set_dred_duration(&mut self, duration: i32) -> Result<()> {
        // SAFETY: self uniquely owns the live encoder; the CTL takes an int.
        let code = unsafe {
            ffi::opus_encoder_ctl(self.ptr, ffi::OPUS_SET_DRED_DURATION_REQUEST, duration)
        };
        check(code, "OPUS_SET_DRED_DURATION").map(|_| ())
    }
}

impl Decoder {
    /// Decoder complexity; 5 enables the neural synthesis required by DRED.
    pub fn set_complexity(&mut self, complexity: i32) -> Result<()> {
        // SAFETY: self uniquely owns the live decoder; the CTL takes an int.
        let code = unsafe {
            ffi::opus_decoder_ctl(self.ptr, ffi::OPUS_SET_COMPLEXITY_REQUEST, complexity)
        };
        check(code, "OPUS_SET_COMPLEXITY").map(|_| ())
    }
}

/// One owned parser and decoded redundancy object. No input slice is retained.
/// Deliberately not Send/Sync; all use remains on the native decoding worker.
pub struct Dred {
    parser: *mut ffi::OpusDREDDecoder,
    data: *mut ffi::OpusDRED,
    rate: u32,
    start: i32,
    end: i32,
}
impl Dred {
    /// Parse at most `samples` of history. None means no compatible redundancy.
    pub fn parse(packet: &[u8], samples: i32, rate: u32) -> Result<Option<Self>> {
        if packet.is_empty()
            || packet.len() > 1500
            || rate != 48000
            || !(1..=2880).contains(&samples)
        {
            return Err(Error::from_code("dred bounds", ffi::OPUS_BAD_ARG));
        }
        let mut error = 0;
        // SAFETY: the output error pointer is valid; allocation is owned below.
        let parser = unsafe { ffi::opus_dred_decoder_create(&mut error) };
        if parser.is_null() {
            return Err(Error::from_code(
                "opus_dred_decoder_create",
                if error < 0 {
                    error
                } else {
                    ffi::OPUS_ALLOC_FAIL
                },
            ));
        }
        // SAFETY: no borrowed inputs; error points to a live integer.
        let data = unsafe { ffi::opus_dred_alloc(&mut error) };
        let mut owned = Self {
            parser,
            data,
            rate,
            start: 0,
            end: 0,
        };
        if data.is_null() {
            return Err(Error::from_code(
                "opus_dred_alloc",
                if error < 0 {
                    error
                } else {
                    ffi::OPUS_ALLOC_FAIL
                },
            ));
        }
        // SAFETY: unique live objects, bounded slice/length and valid output pointer.
        // Processing is immediate; libopus copies features and retains no packet pointer.
        let code = unsafe {
            ffi::opus_dred_parse(
                parser,
                data,
                packet.as_ptr(),
                packet.len() as i32,
                samples,
                rate as i32,
                &mut owned.end,
                0,
            )
        };
        owned.start = check(code, "opus_dred_parse")?;
        Ok((owned.start > 0).then_some(owned))
    }

    /// Decode exactly one 20 ms interval wholly covered by the parsed history.
    /// None leaves the main decoder untouched when that interval is unavailable.
    pub fn decode_float(
        &self,
        decoder: &mut Decoder,
        offset: i32,
        pcm: &mut [f32],
    ) -> Result<Option<usize>> {
        let channels = decoder.channels as usize;
        if pcm.len() != 960 * channels || decoder.get_sample_rate()? != self.rate {
            return Err(Error::from_code("dred output bounds", ffi::OPUS_BAD_ARG));
        }
        if !(960..=2880).contains(&offset)
            || offset % 960 != 0
            || offset > self.start
            || offset - 960 < self.end
        {
            return Ok(None);
        }
        // SAFETY: parsed, live owned DRED; unique decoder and correctly sized output.
        let code = unsafe {
            ffi::opus_decoder_dred_decode_float(
                decoder.ptr,
                self.data,
                offset,
                pcm.as_mut_ptr(),
                960,
            )
        };
        check(code, "opus_decoder_dred_decode_float").map(|n| Some(n as usize))
    }
}
impl Drop for Dred {
    fn drop(&mut self) {
        // SAFETY: allocations have one owner and are freed with their matching APIs.
        unsafe {
            if !self.data.is_null() {
                ffi::opus_dred_free(self.data);
            }
            ffi::opus_dred_decoder_destroy(self.parser);
        }
    }
}
fn check(code: i32, name: &'static str) -> Result<i32> {
    if code < 0 {
        Err(Error::from_code(name, code))
    } else {
        Ok(code)
    }
}
