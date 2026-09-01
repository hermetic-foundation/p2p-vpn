package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.nio.charset.StandardCharsets;
import org.junit.Test;

public final class DeviceHostnameTest {
    @Test
    public void prefersTheUserVisibleDeviceName() {
        assertEquals(
                "midis-pixel",
                DeviceHostname.fromCandidates("Midi's Pixel", "Google", "Pixel 8 Pro"));
    }

    @Test
    public void fallsBackToManufacturerAndModel() {
        assertEquals(
                "google-pixel-8-pro",
                DeviceHostname.fromCandidates(null, "Google", "Pixel 8 Pro"));
        assertEquals(
                "samsung-sm-s928u",
                DeviceHostname.fromCandidates("  ", "Samsung", "Samsung SM-S928U"));
        assertEquals("pixel-8", DeviceHostname.fromCandidates(null, "unknown", "Pixel 8"));
    }

    @Test
    public void normalizesToAnAsciiDnsLabel() {
        assertEquals("midis-cafe-phone-2", DeviceHostname.normalize("  MIDI's Caf\u00e9 Phone #2  "));
        assertEquals("alpha-beta", DeviceHostname.normalize("--Alpha___...Beta--"));
    }

    @Test
    public void truncatesToSixtyThreeAsciiBytesWithoutTrailingHyphens() {
        String input = "a".repeat(62) + " " + "suffix";
        String hostname = DeviceHostname.normalize(input);

        assertEquals("a".repeat(62), hostname);
        assertTrue(hostname.getBytes(StandardCharsets.US_ASCII).length <= 63);
    }

    @Test
    public void replacesUnusableValuesWithTheDefault() {
        assertEquals(DeviceHostname.DEFAULT_HOSTNAME, DeviceHostname.normalize(null));
        assertEquals(DeviceHostname.DEFAULT_HOSTNAME, DeviceHostname.normalize(" \ud83d\udcf1 --- \u96fb\u8a71 "));
        assertEquals(
                DeviceHostname.DEFAULT_HOSTNAME,
                DeviceHostname.fromCandidates(null, "unknown", "---"));
    }
}
