package org.hermeticfoundation.p2pvpn;

import java.util.HashMap;
import java.util.Map;

final class UnderlayTracker {
    enum Change {
        INITIAL,
        UNCHANGED,
        AVAILABLE_CHANGED,
        CHANGED,
        LOST,
        RECOVERED;

        boolean requiresRuntimeRecovery() {
            return this == CHANGED || this == LOST || this == RECOVERED;
        }
    }

    enum Kind {
        NONE("none", -1),
        OTHER("other", 0),
        BLUETOOTH("bluetooth", 100),
        CELLULAR("cellular", 200),
        WIFI("wifi", 300),
        ETHERNET("ethernet", 400);

        final String label;
        final int priority;

        Kind(String label, int priority) {
            this.label = label;
            this.priority = priority;
        }
    }

    static final class Snapshot {
        final String kind;
        final boolean validated;
        final int availableNetworks;
        final long selectionChanges;
        final long selectedLosses;
        final long recoveries;

        private Snapshot(
                String kind,
                boolean validated,
                int availableNetworks,
                long selectionChanges,
                long selectedLosses,
                long recoveries) {
            this.kind = kind;
            this.validated = validated;
            this.availableNetworks = availableNetworks;
            this.selectionChanges = selectionChanges;
            this.selectedLosses = selectedLosses;
            this.recoveries = recoveries;
        }

        static Snapshot empty() {
            return new Snapshot(Kind.NONE.label, false, 0, 0, 0, 0);
        }
    }

    private static final class Candidate {
        final Kind kind;
        final boolean validated;
        final int priority;

        Candidate(Kind kind, boolean validated) {
            this.kind = kind;
            this.validated = validated;
            this.priority = kind.priority + (validated ? 1_000 : 0);
        }
    }

    private final Map<String, Candidate> availableNetworks = new HashMap<>();
    private String currentNetwork;
    private boolean initialized;
    private long selectionChanges;
    private long selectedLosses;
    private long recoveries;

    synchronized Change observe(String network, Kind kind, boolean validated) {
        if (network == null
                || network.isEmpty()
                || kind == null
                || kind == Kind.NONE) {
            return Change.UNCHANGED;
        }
        Candidate existing = availableNetworks.get(network);
        if (existing != null
                && existing.kind == kind
                && existing.validated == validated) {
            return Change.UNCHANGED;
        }
        String previous = currentNetwork;
        boolean selectedCandidateChanged = network.equals(previous) && existing != null;
        availableNetworks.put(network, new Candidate(kind, validated));
        currentNetwork = selectNetwork(previous);
        if (previous == null) {
            if (!initialized) {
                initialized = true;
                return Change.INITIAL;
            }
            selectionChanges = increment(selectionChanges);
            recoveries = increment(recoveries);
            return Change.RECOVERED;
        }
        if (previous.equals(currentNetwork)) {
            if (selectedCandidateChanged) {
                selectionChanges = increment(selectionChanges);
                return Change.CHANGED;
            }
            return Change.AVAILABLE_CHANGED;
        }
        selectionChanges = increment(selectionChanges);
        return Change.CHANGED;
    }

    synchronized Change lost(String network) {
        if (network == null || !availableNetworks.containsKey(network)) {
            return Change.UNCHANGED;
        }
        boolean selected = network.equals(currentNetwork);
        availableNetworks.remove(network);
        if (!selected) {
            return Change.AVAILABLE_CHANGED;
        }
        selectedLosses = increment(selectedLosses);
        selectionChanges = increment(selectionChanges);
        currentNetwork = selectNetwork(null);
        return currentNetwork == null ? Change.LOST : Change.CHANGED;
    }

    synchronized Snapshot snapshot() {
        Candidate selected = availableNetworks.get(currentNetwork);
        if (selected == null) {
            return new Snapshot(
                    Kind.NONE.label,
                    false,
                    availableNetworks.size(),
                    selectionChanges,
                    selectedLosses,
                    recoveries);
        }
        return new Snapshot(
                selected.kind.label,
                selected.validated,
                availableNetworks.size(),
                selectionChanges,
                selectedLosses,
                recoveries);
    }

    private String selectNetwork(String preferred) {
        int maximumPriority = Integer.MIN_VALUE;
        for (Candidate candidate : availableNetworks.values()) {
            maximumPriority = Math.max(maximumPriority, candidate.priority);
        }
        if (maximumPriority == Integer.MIN_VALUE) {
            return null;
        }
        Candidate preferredCandidate = availableNetworks.get(preferred);
        if (preferredCandidate != null && preferredCandidate.priority == maximumPriority) {
            return preferred;
        }
        String selected = null;
        for (Map.Entry<String, Candidate> entry : availableNetworks.entrySet()) {
            if (entry.getValue().priority == maximumPriority
                    && (selected == null || entry.getKey().compareTo(selected) < 0)) {
                selected = entry.getKey();
            }
        }
        return selected;
    }

    private static long increment(long value) {
        return value == Long.MAX_VALUE ? value : value + 1;
    }
}
