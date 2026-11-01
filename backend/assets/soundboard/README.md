# Built-in soundboard sounds

Every server offers these four sounds. The files are the unmodified downloads
listed below. The backend embeds them and, on first use, runs them through the
same pipeline as uploads: mono downmix, silence trim, 48 kHz resampling, a
5-second cap with a short fade and loudness leveling (`backend/src/soundboard`).
Clients only receive that normalized PCM.

All four are dedicated to the public domain under
[CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/): no attribution is
required, and redistribution and modification are allowed. Credits are kept here
anyway. Downloaded 2026-11-01.

| File | Sound | Source | Author | License | SHA-256 |
| --- | --- | --- | --- | --- | --- |
| `cricket.ogg` | "Field cricket" singing (BigSoundBank #1020) | <https://bigsoundbank.com/field-cricket-s1020.html> | Joseph Sardin, BigSoundBank | CC0 1.0 | `74e1f5ad1ccb6e341eeb63bb5a846a7f0dcf2e8e428667e6a3ce9949f3a73e8d` |
| `clap.ogg` | Several people applauding indoors, "Applause #1" (BigSoundBank #2363) | <https://bigsoundbank.com/applause-1-s2363.html> | Dorian Clair, BigSoundBank | CC0 1.0 | `dc07271dc254f171cd96c03ca43d97b182cd4389b833956252179bde0b03a378` |
| `sad-horn.mp3` | "wah wah sad trombone" (Freesound #175409, high-quality preview) | <https://freesound.org/people/kirbydx/sounds/175409/> | kirbydx | CC0 1.0 | `de4a029a0537dbecaf49b4936b0969919f5b7b54083d3859d584b5c4cc7dee3d` |
| `ba-dum-tss.ogg` | Stand-up joke drum fill, "Battery Joke #1" (BigSoundBank #0433) | <https://bigsoundbank.com/battery-joke-s0433.html> | SimonLacelle, BigSoundBank | CC0 1.0 | `56259ce874d75a000847c938013bdff5b764d3a78ab607cbf2cc5d972feb0c07` |

BigSoundBank's license terms: <https://bigsoundbank.com/licenses.html>.
Do not replace these with sounds ripped from other apps or services; use openly
licensed recordings and update this table.
