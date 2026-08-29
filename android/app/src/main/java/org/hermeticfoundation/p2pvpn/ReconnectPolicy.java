package org.hermeticfoundation.p2pvpn;

import java.util.HashMap;
import java.util.Map;

final class ReconnectPolicy {
    enum Change {
        INITIAL,
        UNCHANGED,
        RECONNECT
    }

    private final Map<String, Integer> availableNetworks = new HashMap<>();
    private String currentNetwork;
    private boolean initialized;

    synchronized Change observe(String network, int priority) {
        if (network == null || network.isEmpty() || priority < 0) {
            return Change.UNCHANGED;
        }
        String previous = currentNetwork;
        availableNetworks.put(network, priority);
        currentNetwork = selectNetwork(previous);
        if (previous == null) {
            Change change = initialized ? Change.RECONNECT : Change.INITIAL;
            initialized = true;
            return change;
        }
        if (previous.equals(currentNetwork)) {
            return Change.UNCHANGED;
        }
        return Change.RECONNECT;
    }

    synchronized boolean lost(String network) {
        if (network == null || !availableNetworks.containsKey(network)) {
            return false;
        }
        String previous = currentNetwork;
        availableNetworks.remove(network);
        currentNetwork = selectNetwork(previous);
        return previous != null && !previous.equals(currentNetwork);
    }

    private String selectNetwork(String preferred) {
        int maximumPriority = Integer.MIN_VALUE;
        for (int priority : availableNetworks.values()) {
            maximumPriority = Math.max(maximumPriority, priority);
        }
        if (maximumPriority == Integer.MIN_VALUE) {
            return null;
        }
        if (preferred != null
                && availableNetworks.getOrDefault(preferred, Integer.MIN_VALUE)
                        == maximumPriority) {
            return preferred;
        }
        String selected = null;
        for (Map.Entry<String, Integer> entry : availableNetworks.entrySet()) {
            if (entry.getValue() == maximumPriority
                    && (selected == null || entry.getKey().compareTo(selected) < 0)) {
                selected = entry.getKey();
            }
        }
        return selected;
    }
}
