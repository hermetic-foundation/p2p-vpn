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
                                + networkWithPeerSnapshot(ALPHA, "running", null, 2)
                                + ","
                                + network(BETA, "failed", "isolated failure", 3)
                                + "]}");

        P2pVpnService.RuntimeStatusSnapshot status =
                P2pVpnService.RuntimeStatusSnapshot.from(value, Arrays.asList(ALPHA, BETA));
        RuntimeSummary summary = RuntimeSummary.fromLines(status.metrics);

        assertFalse(status.requiresWholeRuntimeRestart());
        assertTrue(status.networks.get(ALPHA).isAvailable());
        assertTrue(status.networks.get(ALPHA).peerSnapshot.isPresent());
        assertEquals(
                "alpha-device",
                status.networks.get(ALPHA).peerSnapshot.get().peers.get(0).hostnames.get(0));
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
    public void retryingNetworkDoesNotRestartHealthySiblings() throws Exception {
        JSONObject value =
                new JSONObject(
                        "{\"phase\":\"running\","
                                + "\"detail\":\"1 running, 1 starting, 0 failed of 2 networks\","
                                + "\"lines\":[],\"networks\":["
                                + network(ALPHA, "running", null, 1)
                                + ","
                                + network(BETA, "starting", "Recovering in 750 ms", 0)
                                + "]}");

        P2pVpnService.RuntimeStatusSnapshot status =
                P2pVpnService.RuntimeStatusSnapshot.from(value, Arrays.asList(ALPHA, BETA));

        assertFalse(status.requiresWholeRuntimeRestart());
        assertTrue(status.networks.get(ALPHA).isAvailable());
        assertFalse(status.networks.get(BETA).isAvailable());
        assertEquals("Connecting: 1 running, 1 starting", status.describeConnection());
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

    @Test
    public void rejectsMalformedKnownPeerSnapshots() throws Exception {
        String malformedNetwork =
                network(ALPHA, "running", null, 1).replace(
                        "}", ",\"peer_snapshot\":{\"schema_version\":1}}" );
        JSONObject value =
                new JSONObject(
                        "{\"phase\":\"running\",\"detail\":null,\"lines\":[],"
                                + "\"networks\":["
                                + malformedNetwork
                                + "]}");

        assertThrows(
                P2pVpnException.class,
                () ->
                        P2pVpnService.RuntimeStatusSnapshot.from(
                                value, Collections.singletonList(ALPHA)));
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

    private static String networkWithPeerSnapshot(
            String id, String phase, String detail, int peers) throws Exception {
        JSONObject network = new JSONObject(network(id, phase, detail, peers));
        JSONObject peer =
                new JSONObject()
                        .put("peer_id", "alphaPeer")
                        .put("hostnames", new org.json.JSONArray().put("alpha-device"))
                        .put("ipv4", new org.json.JSONArray().put("10.42.0.1"))
                        .put("ipv6", new org.json.JSONArray())
                        .put("local", true)
                        .put(
                                "membership_sources",
                                new org.json.JSONArray().put("local_configuration"))
                        .put("connection_state", "local");
        network.put(
                "peer_snapshot",
                new JSONObject()
                        .put("schema_version", 1)
                        .put("observed_at_unix_seconds", 1_788_291_000L)
                        .put("total_peers", 1)
                        .put("returned_peers", 1)
                        .put("truncated", false)
                        .put("peers", new org.json.JSONArray().put(peer)));
        return network.toString();
    }
}
