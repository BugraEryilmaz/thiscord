# DSP regression fixture

`speech.wav` is synthetic English speech, generated locally using Windows
System.Speech, the default installed voice, 48 kHz mono PCM16. It contains no
microphone recording or personal conversation. Text:

> Please keep my voice clear while the fan is running. We can talk together
> without losing the beginning of a sentence.

This is a deterministic regression input once checked in, not a substitute for
real voices, accents, Turkish speech, rooms or subjective listening acceptance.
The benchmark also accepts a user-supplied WAV without saving or logging audio.
