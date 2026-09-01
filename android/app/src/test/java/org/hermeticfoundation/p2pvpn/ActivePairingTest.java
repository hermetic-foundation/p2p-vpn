package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.json.JSONObject;
import org.junit.Test;

public final class ActivePairingTest {
    private static final String NETWORK_ID = "11111111-1111-4111-8111-111111111111";

    @Test
    public void currentStateRoundTripsItsNetworkIdentity() throws Exception {
        P2pVpnService.ActivePairing original =
                P2pVpnService.ActivePairing.joiner(NETWORK_ID, "operation-1", "ABCD-EFGH");
        original.started = true;

        P2pVpnService.ActivePairing restored =
                P2pVpnService.ActivePairing.fromJson(original.toJson(), null);

        assertEquals(NETWORK_ID, restored.networkId);
        assertEquals("operation-1", restored.operationId);
        assertEquals("ABCD-EFGH", restored.code);
        assertTrue(restored.started);
        assertFalse(restored.needsMigration);
    }

    @Test
    public void legacyStateBindsToTheOnlyAvailableNetwork() throws Exception {
        String legacy =
                "{\"version\":1,\"operation_id\":\"operation-1\","
                        + "\"role\":\"inviter\",\"code\":null,\"started\":false,"
                        + "\"transcript_sha256\":null,\"remote_peer\":null}";

        P2pVpnService.ActivePairing restored =
                P2pVpnService.ActivePairing.fromJson(legacy, NETWORK_ID);
        JSONObject migrated = new JSONObject(restored.toJson());

        assertEquals(NETWORK_ID, restored.networkId);
        assertTrue(restored.needsMigration);
        assertEquals(2, migrated.getInt("version"));
        assertEquals(NETWORK_ID, migrated.getString("network_id"));
    }

    @Test
    public void legacyStateWithoutANetworkIsRejected() {
        String legacy =
                "{\"version\":1,\"operation_id\":\"operation-1\","
                        + "\"role\":\"joiner\",\"code\":\"ABCD-EFGH\","
                        + "\"started\":false,\"transcript_sha256\":null,"
                        + "\"remote_peer\":null}";

        assertThrows(
                P2pVpnException.class,
                () -> P2pVpnService.ActivePairing.fromJson(legacy, null));
    }

    @Test
    public void malformedCurrentNetworkIdentityIsRejected() {
        String malformed =
                "{\"version\":2,\"network_id\":\"not-a-network-id\","
                        + "\"operation_id\":\"operation-1\",\"role\":\"joiner\","
                        + "\"code\":\"ABCD-EFGH\",\"started\":false,"
                        + "\"transcript_sha256\":null,\"remote_peer\":null}";

        assertThrows(
                P2pVpnException.class,
                () -> P2pVpnService.ActivePairing.fromJson(malformed, NETWORK_ID));
    }
}
