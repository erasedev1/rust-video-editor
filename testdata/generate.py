#!/usr/bin/env python3
"""Regenerates the deterministic media fixtures used by ve-media's tests.

Each video frame is a solid colour that encodes its own frame index, so a test
can decode frame N and assert on the colour to prove the seek was frame
accurate rather than merely "a frame came back".

The index is spread across two channels as base-10 digits:

    frame i  ->  R = (i % 10) * 25,  G = (i // 10) * 25,  B = 200

Adjacent frames therefore differ by 25 levels, which is far outside the one or
two levels of error a lossless 4:4:4 encode introduces. A single channel
counting by twos would leave neighbouring frames indistinguishable once codec
rounding is taken into account, and a test that cannot tell frame 9 from frame
10 cannot test seek accuracy at all.

Run from the repository root:  python3 testdata/generate.py
Requires the ffmpeg command-line tool; the generated files are committed, so
contributors do not need it unless they are changing the fixtures.
"""
import subprocess
import sys
from pathlib import Path

OUT = Path(__file__).parent
WIDTH, HEIGHT = 160, 120


MAX_FRAMES = 100  # two base-10 digits


def frame_colour(i):
    assert 0 <= i < MAX_FRAMES, f"frame {i} cannot be encoded in two digits"
    return bytes([(i % 10) * 25, (i // 10) * 25, 200])


def make_video(name, fps_num, fps_den, frames):
    raw = b"".join(frame_colour(i) * (WIDTH * HEIGHT) for i in range(frames))
    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", "rgb24",
        "-s", f"{WIDTH}x{HEIGHT}",
        "-r", f"{fps_num}/{fps_den}",
        "-i", "-",
        # Lossless 4:4:4 so the colour that identifies each frame survives the
        # RGB -> YUV -> RGB round trip intact.
        "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv444p",
        # A short GOP makes the file exercise real seeking rather than letting
        # every request land on a keyframe.
        "-g", "10",
        str(OUT / name),
    ]
    subprocess.run(cmd, input=raw, check=True)
    print(f"  {name}: {frames} frames @ {fps_num}/{fps_den}")


# Amplitude of the generated tone, as a fraction of full scale. Stated
# explicitly via aevalsrc rather than relying on the `sine` source, whose output
# level is not full scale and is not something the tests should have to guess.
TONE_AMPLITUDE = 0.8


def make_audio(name, seconds, rate, freq):
    expr = f"{TONE_AMPLITUDE}*sin(2*PI*{freq}*t)"
    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-f", "lavfi",
        "-i", f"aevalsrc=exprs={expr}|{expr}:s={rate}:d={seconds}",
        "-c:a", "pcm_s16le", "-ac", "2",
        str(OUT / name),
    ]
    subprocess.run(cmd, check=True)
    print(f"  {name}: {seconds}s @ {rate} Hz, amplitude {TONE_AMPLITUDE}")


def make_av(name, seconds, fps, rate):
    frames = int(seconds * fps)
    raw = b"".join(frame_colour(i) * (WIDTH * HEIGHT) for i in range(frames))
    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{WIDTH}x{HEIGHT}",
        "-r", str(fps), "-i", "-",
        "-f", "lavfi",
        "-i", f"aevalsrc=exprs={TONE_AMPLITUDE}*sin(2*PI*440*t)|{TONE_AMPLITUDE}*sin(2*PI*440*t):s={rate}:d={seconds}",
        "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv444p", "-g", "10",
        "-c:a", "aac", "-ac", "2",
        "-shortest",
        str(OUT / name),
    ]
    subprocess.run(cmd, input=raw, check=True)
    print(f"  {name}: {frames} frames + audio")


if __name__ == "__main__":
    if subprocess.run(["which", "ffmpeg"], capture_output=True).returncode != 0:
        sys.exit("the ffmpeg command-line tool is required to regenerate fixtures")
    print("generating fixtures in", OUT)
    make_video("counter_30fps.mp4", 30, 1, 90)
    make_video("counter_2997fps.mp4", 30000, 1001, 60)
    make_video("counter_25fps.mp4", 25, 1, 50)
    make_audio("tone_48k.wav", 1, 48000, 440)
    make_av("av_30fps.mp4", 2, 30, 48000)
    print("done")
