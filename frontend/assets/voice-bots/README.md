# Bot speech library

Twelve distinct, synthetic passages: six English and six Turkish. These are
generated speech, not recordings of real people. The original scripts, installed
voice names and speaking rates are in `scripts.json`. English uses Microsoft
Zira, David and Hazel Desktop; Turkish uses Microsoft Tolga at three rates.
Different Turkish passages are not different human speakers.

The checked-in WAV files are 48 kHz mono PCM16, generated locally with Windows
System.Speech. `generate.ps1` recreates them on a machine with those voices
installed. This optional maintenance script is never run by the build. Playback
on Windows, Linux and macOS uses the bundled assets, with no TTS service, network,
installed voice or extra application package. Assets are embedded only in the
native voice-bot feature, not the WASM UI.

`audio/virtual_devices/speech.rs` normalizes foreground peaks to 0.25, fades the
edges and adds a one-second pause before repeating each passage. It synthesizes
deterministic backgrounds before the normal audio processing pipeline:

- Quiet room: foreground speech only.
- Fan: low filtered noise and a motor hum.
- Keyboard: irregular short, damped noise bursts.
- Café: two overlapping, quieter passages in the same language, with room noise.
- Street: low traffic noise and periodically passing engine hum.
- Rain: steady broadband and filtered noise.

Backgrounds continue through foreground pauses, allowing noise-only VAD and
suppression testing. These are repeatable acoustic approximations, not field
recordings. Microphone scenarios apply on top: Clean adds nothing, Quiet reduces
the whole source, Noise adds further noise, Echo/Silence suppress the loop, and
DoubleTalk adds received speaker audio. All ordinary voice settings still apply.

The legacy `Speech` loop retains the existing DSP regression fixture. Existing
chime, pulse and melody choices also remain available for diagnostic tones.
