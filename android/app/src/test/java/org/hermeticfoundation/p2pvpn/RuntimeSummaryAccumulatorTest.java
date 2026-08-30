package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import java.util.Arrays;
import org.junit.Test;

public final class RuntimeSummaryAccumulatorTest {
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
