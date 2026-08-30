package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

import java.util.Arrays;

public final class RuntimeSummaryTest {
    @Test
    public void parsesPeerAndPathMetrics() {
        RuntimeSummary summary =
                RuntimeSummary.fromLines(
                        Arrays.asList(
                                "path_peers_with_supported_path 2",
                                "path_healthy_direct_udp_datagram_paths 1",
                                "path_healthy_direct_quic_datagram_paths 2",
                                "path_healthy_direct_quic_stream_paths 1",
                                "path_healthy_direct_quic_stream_paths 2",
                                "path_healthy_direct_tcp_stream_paths 1",
                                "path_healthy_relay_paths 3",
                                "public_routing_peers 4",
                                "packet_plane_quic_sessions 5",
                                "outbound_quic_datagram_packets 6"));

        assertEquals(2, summary.connectedPeers);
        assertEquals(7, summary.directPaths);
        assertEquals(1, summary.directUdpDatagramPaths);
        assertEquals(2, summary.directQuicDatagramPaths);
        assertEquals(3, summary.directQuicStreamPaths);
        assertEquals(1, summary.directTcpStreamPaths);
        assertEquals(3, summary.relayPaths);
        assertEquals(4, summary.publicRoutingPeers);
        assertEquals(5, summary.packetPlaneQuicSessions);
        assertEquals(6, summary.outboundQuicDatagramPackets);
    }

    @Test
    public void ignoresMalformedAndNegativeMetrics() {
        RuntimeSummary summary =
                RuntimeSummary.fromLines(
                        Arrays.asList(
                                "missing-value",
                                "path_peers_with_supported_path nope",
                                "path_healthy_relay_paths -1",
                                "packet_plane_quic_sessions -1",
                                "outbound_quic_datagram_packets nope"));

        assertEquals(0, summary.connectedPeers);
        assertEquals(0, summary.relayPaths);
        assertEquals(0, summary.packetPlaneQuicSessions);
        assertEquals(0, summary.outboundQuicDatagramPackets);
    }

    @Test
    public void emptySummaryDefaultsOwnedQuicMetricsToZero() {
        RuntimeSummary summary = RuntimeSummary.empty();

        assertEquals(0, summary.packetPlaneQuicSessions);
        assertEquals(0, summary.outboundQuicDatagramPackets);
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
