Glance 1.0.0 (Windows x64)
==========================

Glance is a small, quick-starting video player, made to replace the Windows
default player. It plays almost any video file, with a minimal overlay that
hides itself while you watch.


REQUIREMENTS
------------
- Windows 10 or 11, 64-bit.
- A graphics driver with OpenGL 3.1 or newer (any normal PC from the last
  decade has this).

Everything else is included: the FFmpeg DLLs in this folder do the decoding.
Glance doesn't install anything and needs no admin rights. Keep the DLLs next
to glance.exe.


MAKE IT YOUR DEFAULT VIDEO PLAYER
---------------------------------
1. Move this folder somewhere permanent, for example C:\Tools\Glance.
2. Run glance.exe once and click "Set as default player". Or press # for
   Settings and use "Set Glance as default video player". From a terminal,
   `glance.exe --register` does the same thing.
3. Windows opens Settings > Default apps. Click "Video player" and choose
   Glance. Windows only lets you make this choice yourself, so this click
   is needed.

To undo it, run `glance.exe --unregister`, then delete the folder. If you move
the folder later, register again.


CONTROLS
--------
  Space / K / click     play / pause
  Left / Right          seek (5 s by default; Ctrl = 6x further)
  Home                  back to the start
  Up / Down / wheel     volume            M   mute
  PageDown / N          next video in the folder
  PageUp / P            previous video in the folder
  F / F11 / Enter /     fullscreen        Esc leaves fullscreen
    double-click
  + / -  (or ] / [)     faster / slower (0.25x-3x, voices keep their pitch)
  Backspace             normal speed
  L                     loop on / off
  I                     media info (codecs, resolution, bitrate, tags...)
  #                     settings
  E                     open in Kerf (once linked, see below)
  Ctrl+O                open a file

Hover over the timeline to see a preview picture of that moment. Click the
time to switch between total and remaining time.


SETTINGS (#)
------------
- Timeline colours (played part, remaining part, handle) and the hover
  preview pictures.
- At the end of a video: stop, loop, or play the next one in the folder.
- Resume videos where you left off. Press Home to start from the beginning.
- Seek and volume step sizes, how quickly the controls hide, remaining-time
  display, keep the window on top.
- Kerf video editor: "Link Kerf..." lets you pick kerf.exe. A Kerf button then
  appears in the controls. It (or E) opens the current video in Kerf at the
  same moment, and Glance pauses.

Settings live in %APPDATA%\Glance\settings.ini. Resume positions are stored in
positions.txt in the same folder.


KNOWN LIMITS
------------
- HDR videos play without tone mapping, so their colours look washed out.
- Subtitles aren't shown, and only the main video and audio tracks play.
- Decoding is done by the CPU. 4K HEVC or AV1 needs a reasonably fast
  processor.


LICENSES
--------
Glance is open source under the GNU GPL v3 or later (see LICENSE.txt).
Source code and new releases: https://github.com/TheSnapped/Glance
Glance was built with the help of AI (Claude by Anthropic).

Glance uses FFmpeg (https://ffmpeg.org) under the GNU LGPL. The DLLs are the
unmodified "n9.0 lgpl-shared" build from https://github.com/BtbN/FFmpeg-Builds.
Its source is at https://github.com/FFmpeg/FFmpeg (branch release/9.0).
See FFMPEG-LICENSE.txt. You may replace the DLLs with your own compatible
FFmpeg 9 build.
