package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.util.Arrays;
import java.util.Collections;
import org.json.JSONObject;
import org.junit.Test;

public final class RuntimeStatusSnapshotTest {
    private static final String ALPHA = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    private static final String BETA = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    @Test
    public void keepsHealthyNetworkRunningAndAggregatesChildMetrics() throws Exception {
        JSONObject value =
                new JSONObject(
                        "{\"phase\":\"running\","
                                + "\"detail\":\"1 running, 0 starting, 1 failed of 2 networks\","
                                + "\"lines\":[\"android_supervisor_networks 2\"],"
                                + "\"networks\":["
                                + network(ALPHA, "running", null, 2)
                                + ","
                                + network(BETA, "failed", "isolated failure", 3)
                                + "]}");

        P2pVpnService.RuntimeStatusSnapshot status =
                P2pVpnService.RuntimeStatusSnapshot.from(value, Arrays.asList(ALPHA, BETA));
        RuntimeSummary summary = RuntimeSummary.fromLines(status.metrics);

        assertFalse(status.requiresWholeRuntimeRestart());
        assertTrue(status.networks.get(ALPHA).isAvailable());
        assertFalse(status.networks.get(BETA).isAvailable());
        assertEquals(5, summary.connectedPeers);
        assertEquals("Connected: 1 running, 0 starting, 1 unavailable", status.describeConnection());
    }

    @Test
    public void sharedTunFailureRequiresWholeRuntimeRestart() throws Exception {
        JSONObject value =
                new JSONObject(
                        "{\"phase\":\"failed\",\"detail\":\"shared TUN failed\","
                                + "\"lines\":[],\"networks\":["
                                + network(ALPHA, "running", null, 1)
                                + "]}");

        P2pVpnService.RuntimeStatusSnapshot status =
                P2pVpnService.RuntimeStatusSnapshot.from(
                        value, Collections.singletonList(ALPHA));

        assertTrue(status.requiresWholeRuntimeRestart());
    }

    @Test
    public void rejectsMissingOrUnexpectedNetworks() throws Exception {
        JSONObject value =
                new JSONObject(
                        "{\"phase\":\"running\",\"detail\":null,\"lines\":[],"
                                + "\"networks\":["
                                + network(ALPHA, "running", null, 1)
                                + "]}");

        assertThrows(
                P2pVpnException.class,
                () ->
                        P2pVpnService.RuntimeStatusSnapshot.from(
                                value, Arrays.asList(ALPHA, BETA)));
    }

    private static String network(String id, String phase, String detail, int peers) {
        String encodedDetail = detail == null ? "null" : JSONObject.quote(detail);
        return "{\"id\":"
                + JSONObject.quote(id)
                + ",\"phase\":"
                + JSONObject.quote(phase)
                + ",\"detail\":"
                + encodedDetail
                + ",\"lines\":[\"path_peers_with_supported_path "
                + peers
                + "\"]}";
    }
}
