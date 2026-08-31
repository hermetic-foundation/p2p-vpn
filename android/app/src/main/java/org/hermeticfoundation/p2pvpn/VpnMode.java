package org.hermeticfoundation.p2pvpn;

import java.util.Objects;

final class VpnMode {
    final boolean alwaysOn;
    final boolean lockdown;

    private VpnMode(boolean alwaysOn, boolean lockdown) {
        this.alwaysOn = alwaysOn;
        this.lockdown = lockdown;
    }

    static VpnMode manual() {
        return new VpnMode(false, false);
    }

    static VpnMode resolve(
            int androidApi,
            boolean systemStarted,
            boolean platformAlwaysOn,
            boolean platformLockdown) {
        if (androidApi >= 29) {
            boolean lockdown = platformLockdown;
            return new VpnMode(platformAlwaysOn || lockdown, lockdown);
        }
        return new VpnMode(systemStarted, false);
    }

    boolean permitsDisconnect() {
        return !alwaysOn;
    }

    boolean permitsOverlayConnection() {
        return !lockdown;
    }

    @Override
    public boolean equals(Object other) {
        if (this == other) {
            return true;
        }
        if (!(other instanceof VpnMode)) {
            return false;
        }
        VpnMode mode = (VpnMode) other;
        return alwaysOn == mode.alwaysOn && lockdown == mode.lockdown;
    }

    @Override
    public int hashCode() {
        return Objects.hash(alwaysOn, lockdown);
    }
}
