Feature: Pure-Rust media probing of placed files (C20 media-info, SKADI-I-0033)
  After a file lands, the hunter probes its real streams (resolution, codec,
  duration, audio) so the stored quality reflects the file, not the title.
  Best-effort: unsupported or corrupt files yield nothing, never an error.
  Mirrors Sonarr/Radarr MediaInfo.

  @C20 @passing
  Scenario: an MP3 reports codec, channels, sample rate, bitrate and duration
    Given the fixture "tiny.mp3"
    When the file is probed
    Then the file has no video stream
    And the audio codec is "mp3"
    And the audio has 2 channels at 44100 Hz
    And the audio bitrate is at least 96 kbps
    And the duration is 1 second

  @C20 @passing
  Scenario: an M4A reports AAC audio
    Given the fixture "tiny.m4a"
    When the file is probed
    Then the file has no video stream
    And the audio codec is "aac"
    And the audio has 2 channels at 44100 Hz

  @C20 @passing
  Scenario: an M4B audiobook goes through the audio reader
    Given the fixture "tiny.m4a" copied to a file named "book.m4b"
    When the file is probed
    Then the file has no video stream
    And the audio codec is "aac"

  @C20 @passing
  Scenario: an MP4 reports video dimensions, codec, tier and its audio track
    Given the fixture "tiny.mp4"
    When the file is probed
    Then the video is 320x240 h264
    And the resolution tier is "SD"
    And the audio codec is "aac"
    And the duration is 1 second

  @C20 @passing
  Scenario: an MKV reports video dimensions, codec and its audio track
    Given the fixture "tiny.mkv"
    When the file is probed
    Then the video is 320x240 h264
    And the audio codec is "aac"
    And the audio has 2 channels at 44100 Hz

  @C20 @passing
  Scenario: a synthesised WAV is read through the audio reader
    Given a synthesised 44100 Hz mono 16-bit WAV of 1 second named "tone.wav"
    When the file is probed
    Then the audio codec is "pcm"
    And the audio has 1 channel at 44100 Hz
    And the duration is 1 second

  @C20 @passing
  Scenario: the extension is matched case-insensitively
    Given the fixture "tiny.mkv" copied to a file named "MOVIE.MKV"
    When the file is probed
    Then the video is 320x240 h264

  @C20 @passing
  Scenario: resolution tiers follow the *arr height thresholds, letterbox-aware
    Then a 3840x2160 frame is tier "2160p"
    And a 1920x1080 frame is tier "1080p"
    And a 1920x800 frame is tier "1080p"
    And a 1280x720 frame is tier "720p"
    And a 1280x534 frame is tier "720p"
    And a 854x480 frame is tier "480p"
    And a 640x360 frame is tier "SD"

  @C20 @passing
  Scenario: an unknown extension probes to nothing
    Given the fixture "tiny.mkv" copied to a file named "notes.txt"
    When the file is probed
    Then it probes to nothing

  @C20 @passing
  Scenario: a missing file probes to nothing rather than erroring
    Given a file named "gone.mkv" that does not exist
    When the file is probed
    Then it probes to nothing

  @C20 @passing
  Scenario Outline: corrupt or empty media files probe to nothing and never panic
    Given a file named "<name>" containing <bytes> bytes of garbage
    When the file is probed
    Then it probes to nothing

    Examples:
      | name        | bytes  |
      | junk.mkv    | 4096   |
      | junk.mp4    | 4096   |
      | junk.mp3    | 4096   |
      | junk.m4b    | 4096   |
      | junk.flac   | 4096   |
      | junk.webm   | 65536  |
      | junk.m4v    | 65536  |

  @C20 @passing
  Scenario: an empty file probes to nothing
    Given an empty file named "empty.mkv"
    When the file is probed
    Then it probes to nothing

  @C20 @passing
  Scenario: a truncated real container probes to nothing rather than a partial answer
    Given a file named "trunc.mp4" containing 100 bytes of garbage
    When the file is probed
    Then it probes to nothing

  # Still a gap (SKADI-T-0422): Matroska has no per-track bitrate element — it is
  # derived from stream size, which the header does not carry. Needs either a
  # demuxing pass or a different library; see SKADI-T-0543.
  @C20 @gap
  Scenario: the audio bitrate of an MKV is reported (Sonarr MediaInfo AudioBitrate)
    Given the fixture "tiny.mkv"
    When the file is probed
    Then the audio bitrate is known

  @C20 @passing @SKADI-T-0422
  Scenario: the audio channel count of an MP4 is reported (Sonarr MediaInfo AudioChannels)
    Given the fixture "tiny.mp4"
    When the file is probed
    Then the audio channel count is known

  # Still a gap for *video* (SKADI-T-0422): the `matroska` crate does not expose
  # the Colour element, so HDR/transfer characteristics cannot be read without
  # hand-parsing it. Audio bit depth IS now reported. See SKADI-T-0543.
  @C20 @gap
  Scenario: HDR / dynamic range and bit depth are reported (Sonarr MediaInfo VideoDynamicRange)
    Given the fixture "tiny.mkv"
    When the file is probed
    Then the video dynamic range is known

  @C20 @passing @SKADI-T-0422
  Scenario: audio and subtitle languages are reported (Sonarr MediaInfo AudioLanguages)
    Given the fixture "tiny.mkv"
    When the file is probed
    Then the audio languages are known
