package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import java.util.Arrays;

import org.junit.Test;

public final class RuntimeDiagnosticsTest {
    @Test
    public void parsesOnlyBoundedAggregateHealthMetrics() {
        RuntimeDiagnostics diagnostics =
                RuntimeDiagnostics.fromLines(
                        Arrays.asList(
                                "path_peers_without_supported_path 1",
                                "queue_queued_packets 2",
                                "queue_queued_bytes 3",
                                "queue_oldest_packet_age_millis 4",
                                "queue_dropped_packets 5",
                                "queue_dropped_bytes 6",
                                "queue_expired_packets 7",
                                "queue_expired_bytes 8",
                                "outbound_dropped_packets 9",
                                "inbound_dropped_packets 10",
                                "packet_plane_inbound_dropped_datagrams 11",
                                "path_fallbacks_to_relay 12",
                                "packet_plane_path_demotions 13",
                                "stream_fallback_path_demotions 14",
                                "outbound_queue_blocked_no_supported_path_events 15",
                                "outbound_queue_blocked_packet_window_events 16",
                                "peer_secret 999"));

        assertEquals(1, diagnostics.peersWithoutSupportedPath);
        assertEquals(2, diagnostics.queueQueuedPackets);
        assertEquals(3, diagnostics.queueQueuedBytes);
        assertEquals(4, diagnostics.queueOldestPacketAgeMillis);
        assertEquals(5, diagnostics.queueDroppedPackets);
        assertEquals(6, diagnostics.queueDroppedBytes);
        assertEquals(7, diagnostics.queueExpiredPackets);
        assertEquals(8, diagnostics.queueExpiredBytes);
        assertEquals(9, diagnostics.outboundDroppedPackets);
        assertEquals(10, diagnostics.inboundDroppedPackets);
        assertEquals(11, diagnostics.packetPlaneInboundDroppedDatagrams);
        assertEquals(12, diagnostics.pathFallbacksToRelay);
        assertEquals(13, diagnostics.packetPlanePathDemotions);
        assertEquals(14, diagnostics.streamFallbackPathDemotions);
        assertEquals(15, diagnostics.blockedNoSupportedPathEvents);
        assertEquals(16, diagnostics.blockedPacketWindowEvents);
    }

    @Test
    public void malformedAndNegativeValuesRemainZero() {
        RuntimeDiagnostics diagnostics =
                RuntimeDiagnostics.fromLines(
                        Arrays.asList(
                                "queue_queued_packets nope",
                                "queue_dropped_packets -1",
                                "outbound_dropped_packets",
                                "inbound_dropped_packets 4 5"));

        assertEquals(0, diagnostics.queueQueuedPackets);
        assertEquals(0, diagnostics.queueDroppedPackets);
        assertEquals(0, diagnostics.outboundDroppedPackets);
        assertEquals(0, diagnostics.inboundDroppedPackets);
    }
}
