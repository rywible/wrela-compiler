"""CPU excerpt processing must preserve samples and leave the source unchanged."""

import math
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import listening


class ExcerptTest(unittest.TestCase):
    def test_fades_match_full_frame_processing(self):
        for rate in (11025, 48000):
            for channels in (1, 2):
                samples = [math.sin(i * 0.01) for i in range(rate * channels)]
                original = samples.copy()
                # Short cuts overlap the fades; long cuts have an untouched middle.
                for start, end in ((0, 0.01), (0.1, 0.3), (0, 1), (0.9, 2), (2, 3)):
                    with self.subTest(rate=rate, channels=channels, start=start, end=end):
                        expected = samples[int(start * rate) * channels:min(int(end * rate) * channels, len(samples))]
                        frames = len(expected) // channels
                        for i in range(frames):
                            gain = min(1.0, i / rate / listening.FADE_IN, (frames - 1 - i) / rate / listening.FADE_OUT)
                            for c in range(channels):
                                expected[i * channels + c] *= gain
                        self.assertEqual(listening.cut(samples, channels, rate, start, end), expected)
                self.assertEqual(samples, original)
