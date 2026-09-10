package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import java.util.Arrays;
import org.junit.Test;

public final class RuntimeSummaryAccumulatorTest {
    @Test
    public void ownedBackendCountersDescribeCurrentRuntimeNotLifetimeTotals() {
        RuntimeSummaryAccumulator accumulator = new RuntimeSummaryAccumulator();
        RuntimeSummary current = RuntimeSummary.fromLines(Arrays.asList(
                "outbound_quic_datagram_packets 12",
                "outbound_owned_quic_datagram_packets 5",
                "outbound_owned_udp_datagram_packets 7"));
        RuntimeSummary observed = accumulator.observe(current);
        assertEquals(5, observed.outboundOwnedQuicDatagramPackets);
        assertEquals(7, observed.outboundOwnedUdpDatagramPackets);

        RuntimeSummary stopped = accumulator.finishRuntime();
        assertEquals(12, stopped.outboundQuicDatagramPackets);
        assertEquals(0, stopped.outboundOwnedQuicDatagramPackets);
        assertEquals(0, stopped.outboundOwnedUdpDatagramPackets);

        RuntimeSummary restarted = accumulator.observe(current);
        assertEquals(24, restarted.outboundQuicDatagramPackets);
        assertEquals(5, restarted.outboundOwnedQuicDatagramPackets);
        assertEquals(7, restarted.outboundOwnedUdpDatagramPackets);

        RuntimeSummary implicitReset = accumulator.observe(summary(1));
        assertEquals(25, implicitReset.outboundQuicDatagramPackets);
        assertEquals(0, implicitReset.outboundOwnedQuicDatagramPackets);
        assertEquals(0, implicitReset.outboundOwnedUdpDatagramPackets);
    }

    @Test
    public void preservesCountersAcrossExplicitRuntimeRestart() {
        RuntimeSummaryAccumulator accumulator = new RuntimeSummaryAccumulator();

        assertEquals(3, accumulator.observe(summary(3)).outboundQuicDatagramPackets);
        assertEquals(7, accumulator.observe(summary(7)).outboundQuicDatagramPackets);
        assertEquals(7, accumulator.finishRuntime().outboundQuicDatagramPackets);
        assertEquals(9, accumulator.observe(summary(2)).outboundQuicDatagramPackets);
    }

    @Test
    public void detectsCounterResetWithoutExplicitStop() {
        RuntimeSummaryAccumulator accumulator = new RuntimeSummaryAccumulator();

        assertEquals(8, accumulator.observe(summary(8)).outboundQuicDatagramPackets);
        assertEquals(9, accumulator.observe(summary(1)).outboundQuicDatagramPackets);
    }

    @Test
    public void cumulativeCounterSaturates() {
        RuntimeSummaryAccumulator accumulator = new RuntimeSummaryAccumulator();

        accumulator.observe(summary(Long.MAX_VALUE));
        accumulator.finishRuntime();
        assertEquals(
                Long.MAX_VALUE,
                accumulator.observe(summary(1)).outboundQuicDatagramPackets);
    }

    private static RuntimeSummary summary(long packets) {
        return RuntimeSummary.fromLines(
                Arrays.asList(
                        "path_peers_with_supported_path 1",
                        "path_healthy_direct_quic_datagram_paths 1",
                        "packet_plane_quic_sessions 1",
                        "outbound_quic_datagram_packets " + packets));
    }
}
