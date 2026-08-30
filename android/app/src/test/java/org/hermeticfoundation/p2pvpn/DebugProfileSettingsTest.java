package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.fail;

import org.junit.Test;

public final class DebugProfileSettingsTest {
    @Test
    public void acceptsAbsentOrCompleteOwnedQuicSettings() throws Exception {
        P2pVpnService.validateDebugE2ePaths(null, null, null);
        P2pVpnService.validateDebugE2ePaths(
                "0.0.0.0:51821", "127.0.0.1:51821", null);
        P2pVpnService.validateDebugE2ePaths(
                null,
                null,
                "/ip4/10.0.2.2/tcp/42300/p2p/12D3KooWRelay/p2p-circuit");

        assertEquals(
                "0.0.0.0:51821",
                P2pVpnService.boundedOptionalDebugSetting(
                        " 0.0.0.0:51821 ", "packet_quic_listen", 512));
    }

    @Test
    public void rejectsIncompleteOwnedQuicSettings() throws Exception {
        assertIncomplete("0.0.0.0:51821", null);
        assertIncomplete(null, "127.0.0.1:51821");
    }

    @Test
    public void rejectsCombinedOwnedQuicAndRelaySettings() throws Exception {
        try {
            P2pVpnService.validateDebugE2ePaths(
                    "0.0.0.0:51821",
                    "127.0.0.1:51821",
                    "/ip4/10.0.2.2/tcp/42300/p2p/12D3KooWRelay/p2p-circuit");
            fail("expected conflicting E2E path settings to fail");
        } catch (P2pVpnException error) {
            assertFalse(error.getMessage().contains("10.0.2.2"));
        }
    }

    @Test
    public void rejectsEmptyOversizedOrControlCharacterSettings() throws Exception {
        assertInvalid("", 512);
        assertInvalid("a".repeat(513), 512);
        assertInvalid("127.0.0.1:51821\nignored", 512);
    }

    private static void assertIncomplete(String listen, String externalEndpoint) throws Exception {
        try {
            P2pVpnService.validateDebugE2ePaths(listen, externalEndpoint, null);
            fail("expected incomplete owned QUIC settings to fail");
        } catch (P2pVpnException error) {
            assertFalse(error.getMessage().contains(String.valueOf(listen)));
            assertFalse(error.getMessage().contains(String.valueOf(externalEndpoint)));
        }
    }

    private static void assertInvalid(String value, int maximumLength) throws Exception {
        try {
            P2pVpnService.boundedOptionalDebugSetting(
                    value, "packet_quic_external_endpoint", maximumLength);
            fail("expected invalid owned QUIC setting to fail");
        } catch (P2pVpnException error) {
            if (!value.isEmpty()) {
                assertFalse(error.getMessage().contains(value));
            }
        }
    }
}
