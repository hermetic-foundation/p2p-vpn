package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import java.util.Collections;
import org.junit.Test;

public final class AppNavigationTest {
    @Test
    public void addWorkflowUsesNestedBackNavigation() {
        AppNavigation add = AppNavigation.home().openAdd();
        AppNavigation create = add.openCreate();
        AppNavigation join = add.openJoin();

        assertEquals(AppNavigation.Screen.ADD, add.screen);
        assertEquals(AppNavigation.Screen.CREATE, create.screen);
        assertEquals(AppNavigation.Screen.ADD, create.back().screen);
        assertEquals(AppNavigation.Screen.JOIN, join.screen);
        assertEquals(AppNavigation.Screen.ADD, join.back().screen);
        assertEquals(AppNavigation.Screen.HOME, add.back().screen);
    }

    @Test
    public void detailStateRestoresAndFallsBackWhenItsNetworkDisappears() {
        AppNavigation detail = AppNavigation.detail("network-id");
        AppNavigation restored = AppNavigation.restore("DETAIL", "network-id");

        assertEquals(AppNavigation.Screen.DETAIL, restored.screen);
        assertEquals("network-id", restored.networkId);
        assertEquals(
                AppNavigation.Screen.HOME,
                detail.reconcile(Collections.emptyList()).screen);
    }

    @Test
    public void invalidSavedStateFailsBackToHome() {
        AppNavigation unknown = AppNavigation.restore("NOT_A_SCREEN", "network-id");
        AppNavigation missingDetail = AppNavigation.restore("DETAIL", null);

        assertEquals(AppNavigation.Screen.HOME, unknown.screen);
        assertEquals(AppNavigation.Screen.HOME, missingDetail.screen);
        assertNull(unknown.networkId);
    }
}
