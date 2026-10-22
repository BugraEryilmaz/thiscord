# Optional asset maintenance only; neither builds nor playback require Windows TTS.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$entries = Get-Content (Join-Path $PSScriptRoot 'scripts.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$synth = [System.Speech.Synthesis.SpeechSynthesizer]::new()
try {
    $format = [System.Speech.AudioFormat.SpeechAudioFormatInfo]::new(48000,
        [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
        [System.Speech.AudioFormat.AudioChannel]::Mono)
    foreach ($entry in $entries) {
        $synth.SelectVoice($entry.voice)
        $synth.Rate = $entry.rate
        $synth.SetOutputToWaveFile((Join-Path $PSScriptRoot ($entry.id + '.wav')), $format)
        $synth.Speak($entry.text)
        $synth.SetOutputToNull()
    }
} finally { $synth.Dispose() }
