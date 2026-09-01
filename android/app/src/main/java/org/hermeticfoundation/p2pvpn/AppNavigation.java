package org.hermeticfoundation.p2pvpn;

import java.util.List;

final class AppNavigation {
    enum Screen {
        HOME,
        ADD,
        CREATE,
        JOIN,
        DETAIL
    }

    final Screen screen;
    final String networkId;

    private AppNavigation(Screen screen, String networkId) {
        this.screen = screen;
        this.networkId = networkId;
    }

    static AppNavigation home() {
        return new AppNavigation(Screen.HOME, null);
    }

    static AppNavigation restore(String encodedScreen, String networkId) {
        if (encodedScreen == null) {
            return home();
        }
        try {
            Screen restored = Screen.valueOf(encodedScreen);
            if (restored == Screen.DETAIL) {
                return networkId == null || networkId.isEmpty() ? home() : detail(networkId);
            }
            return new AppNavigation(restored, null);
        } catch (IllegalArgumentException error) {
            return home();
        }
    }

    AppNavigation openAdd() {
        return new AppNavigation(Screen.ADD, null);
    }

    AppNavigation openCreate() {
        return new AppNavigation(Screen.CREATE, null);
    }

    AppNavigation openJoin() {
        return new AppNavigation(Screen.JOIN, null);
    }

    static AppNavigation detail(String networkId) {
        if (networkId == null || networkId.isEmpty()) {
            throw new IllegalArgumentException("Network detail requires an ID");
        }
        return new AppNavigation(Screen.DETAIL, networkId);
    }

    AppNavigation back() {
        switch (screen) {
            case CREATE:
            case JOIN:
                return openAdd();
            case ADD:
            case DETAIL:
                return home();
            case HOME:
            default:
                return this;
        }
    }

    AppNavigation reconcile(List<String> networkIds) {
        if (screen != Screen.DETAIL) {
            return this;
        }
        for (String existingNetworkId : networkIds) {
            if (existingNetworkId.equals(networkId)) {
                return this;
            }
        }
        return home();
    }
}
