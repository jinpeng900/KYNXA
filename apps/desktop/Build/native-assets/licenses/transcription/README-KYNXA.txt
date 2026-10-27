KYNXA local transcription runtime

whisper.cpp v1.8.3 (official Windows x64 CPU release), MIT.
OpenAI Whisper tiny multilingual model, GGML conversion from ggerganov/whisper.cpp, MIT.
manifest.json records the pinned sources and verified SHA-256 values.

Put this directory at <application>/runtime/transcription.
WAV, MP3, FLAC and OGG are decoded by the bundled official CLI.
Other supported containers need an audio-enabled FFmpeg. The Playwright recorder FFmpeg is not an audio converter.
Host overrides (never accepted from a web-page request):
  KYNXA_TRANSCRIPTION_ENGINE = absolute whisper-cli executable
  KYNXA_TRANSCRIPTION_MODEL  = absolute compatible GGML model
  KYNXA_FFMPEG_CLI           = absolute audio-enabled ffmpeg executable

Transcription runs offline in an isolated temporary working directory with an authorized byte copy.
It uses two CPU threads, below-normal Windows priority, one concurrent job, a 10-minute processing timeout,
and bounded output. Explicit cancellation is scoped to conversationId + operationId.
FFmpeg conversion handles at most the first 10 minutes and reports that coverage explicitly.
Native WAV/MP3/FLAC/OGG inputs are transcribed completely if they fit the processing/output limits.
Tiny model transcripts require human review, especially names, numbers and noisy speech.
