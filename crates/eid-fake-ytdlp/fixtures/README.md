# Fixtures of the fake yt-dlp (test only, never shipped)

Synthetic media generated from FFmpeg's built-in `lavfi` test sources (`testsrc2` pattern and a
440 Hz sine): no third-party content, no rights of others.

| File | SHA-256 | Content |
|---|---|---|
| `clip.mp4` | `40df818f303c01b7a110e8f8a59a6a442f7f39041cfdce76533962611dad67b9` | 2 s, 160x90 @ 10 fps mpeg4 video + 22.05 kHz mono AAC 32 kb/s (37 030 bytes) |
| `clip.mp3` | `364397438bf967720de9ed65f98dcdee20de65846a8828858fd190eba42adb11` | 2 s, 22.05 kHz mono MP3 32 kb/s (8 255 bytes) |

Generated on 2026-11-15 (Windows 10) with FFmpeg `n8.1.3-35-gf1acb0a266-20261114` from
BtbN/FFmpeg-Builds tag `autobuild-2026-11-14-12-37`, archive
`ffmpeg-n8.1.3-35-gf1acb0a266-win64-lgpl-8.1.zip` (SHA-256
`9a1c8c9f389281749b8d93bfcfaf2164cf95fb74101560ba93a0ec92e1f9e56c`, checked against that tag's
`checksums.sha256`; `ffmpeg.exe` `260c6a993e401526445493cfd3e82edceb578e18da9388f9ff3755310124b027`).
The output is bit-exact: running the command twice gave the same SHA-256.

```
ffmpeg -f lavfi -i "testsrc2=size=160x90:rate=10:duration=2" -f lavfi -i "sine=frequency=440:sample_rate=22050:duration=2" -map 0:v -map 1:a -c:v mpeg4 -q:v 8 -c:a aac -b:a 32k -ac 1 -shortest -map_metadata -1 -fflags +bitexact -flags:v +bitexact -flags:a +bitexact -movflags +faststart clip.mp4
ffmpeg -f lavfi -i "sine=frequency=440:sample_rate=22050:duration=2" -c:a libmp3lame -b:a 32k -ac 1 -map_metadata -1 -fflags +bitexact -flags:a +bitexact -write_xing 0 -id3v2_version 0 clip.mp3
```

`ffprobe` of `clip.mp4`: `codec_name=mpeg4|codec_type=video|duration=2.000000` and
`codec_name=aac|codec_type=audio|duration=2.000000`; of `clip.mp3`: `codec_name=mp3|codec_type=audio`.
The pinned hashes are checked by `tests/fixtures.rs`.
