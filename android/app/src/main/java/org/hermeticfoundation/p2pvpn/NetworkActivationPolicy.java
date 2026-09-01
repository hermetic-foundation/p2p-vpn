package org.hermeticfoundation.p2pvpn;

final class NetworkActivationPolicy {
    enum Outcome {
        CONNECT,
        IDLE_ALWAYS_ON,
        STOP
    }

    private NetworkActivationPolicy() {}

    static Outcome afterMutation(int enabledNetworks, boolean alwaysOn) {
        if (enabledNetworks < 0 || enabledNetworks > ProfileCollection.MAX_NETWORKS) {
            throw new IllegalArgumentException("Invalid enabled network count");
        }
        if (enabledNetworks > 0) {
            return Outcome.CONNECT;
        }
        return alwaysOn ? Outcome.IDLE_ALWAYS_ON : Outcome.STOP;
    }
}
