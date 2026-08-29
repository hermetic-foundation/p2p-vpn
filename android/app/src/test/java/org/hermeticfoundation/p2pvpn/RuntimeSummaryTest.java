package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import java.util.Arrays;
import org.junit.Test;

public final class RuntimeSummaryTest {
    @Test
    public void parsesPeerAndPathMetrics() {
        RuntimeSummary summary =
                RuntimeSummary.fromLines(
                        Arrays.asList(
                                "path_peers_with_supported_path 2",
                                "path_healthy_direct_quic_stream_paths 1",
                                "path_healthy_direct_tcp_stream_paths 1",
                                "path_healthy_relay_paths 3",
                                "public_routing_peers 4"));

        assertEquals(2, summary.connectedPeers);
        assertEquals(2, summary.directPaths);
        assertEquals(3, summary.relayPaths);
        assertEquals(4, summary.publicRoutingPeers);
    }

    @Test
    public void ignoresMalformedAndNegativeMetrics() {
        RuntimeSummary summary =
                RuntimeSummary.fromLines(
                        Arrays.asList(
                                "missing-value",
                                "path_peers_with_supported_path nope",
                                "path_healthy_relay_paths -1"));

        assertEquals(0, summary.connectedPeers);
        assertEquals(0, summary.relayPaths);
    }

    @Test
    public void descriptionDistinguishesOverlayPeersFromPublicRouters() {
        RuntimeSummary summary =
                RuntimeSummary.fromLines(
                        Arrays.asList(
                                "path_peers_with_supported_path 1",
                                "path_healthy_direct_quic_datagram_paths 1",
                                "public_routing_peers 7"));

        assertEquals(
                "Overlay peers: 1 connected; paths: 1 direct, 0 relay; public routers: 7",
                summary.describe());
    }
}
