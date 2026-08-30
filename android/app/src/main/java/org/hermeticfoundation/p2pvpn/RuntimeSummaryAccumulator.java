package org.hermeticfoundation.p2pvpn;

final class RuntimeSummaryAccumulator {
    private long completedOutboundQuicDatagramPackets;
    private long currentOutboundQuicDatagramPackets;

    RuntimeSummary observe(RuntimeSummary current) {
        if (current.outboundQuicDatagramPackets < currentOutboundQuicDatagramPackets) {
            completeCurrentRuntime();
        }
        currentOutboundQuicDatagramPackets = current.outboundQuicDatagramPackets;
        return current.withOutboundQuicDatagramPackets(totalPackets());
    }

    RuntimeSummary finishRuntime() {
        completeCurrentRuntime();
        return RuntimeSummary.empty()
                .withOutboundQuicDatagramPackets(completedOutboundQuicDatagramPackets);
    }

    private void completeCurrentRuntime() {
        completedOutboundQuicDatagramPackets =
                saturatingAdd(
                        completedOutboundQuicDatagramPackets,
                        currentOutboundQuicDatagramPackets);
        currentOutboundQuicDatagramPackets = 0;
    }

    private long totalPackets() {
        return saturatingAdd(
                completedOutboundQuicDatagramPackets, currentOutboundQuicDatagramPackets);
    }

    private static long saturatingAdd(long left, long right) {
        if (Long.MAX_VALUE - left < right) {
            return Long.MAX_VALUE;
        }
        return left + right;
    }
}
