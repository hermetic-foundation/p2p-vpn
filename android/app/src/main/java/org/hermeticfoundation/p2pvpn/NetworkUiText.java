package org.hermeticfoundation.p2pvpn;

import android.content.Context;

final class NetworkUiText {
    private NetworkUiText() {}

    static String display(Context context, NetworkUiState state) {
        int label;
        switch (state.kind) {
            case DISABLED:
                label = R.string.state_disabled;
                break;
            case STARTING:
                label = R.string.state_starting;
                break;
            case CONNECTED:
                label = R.string.state_connected;
                break;
            case DEGRADED:
                label = R.string.state_degraded;
                break;
            case RECOVERING:
                label = R.string.state_recovering;
                break;
            default:
                throw new IllegalStateException("Unsupported network state");
        }
        String stateLabel = context.getString(label);
        return state.detail.isEmpty()
                ? stateLabel
                : context.getString(R.string.state_with_detail, stateLabel, state.detail);
    }
}
