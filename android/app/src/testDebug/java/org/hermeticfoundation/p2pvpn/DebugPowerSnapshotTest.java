package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.*;

import android.content.BroadcastReceiver;
import android.content.ComponentName;
import android.content.ContextWrapper;
import android.content.Intent;
import android.content.IntentFilter;

import org.json.JSONException;
import org.json.JSONObject;
import org.junit.Test;

public final class DebugPowerSnapshotTest {
    private static DebugPowerSnapshot.Reading reading() {
        DebugPowerSnapshot.Reading reading = new DebugPowerSnapshot.Reading();
        reading.startedElapsedMillis = 100_000;
        reading.finishedElapsedMillis = 100_010;
        reading.uptimeMillis = 40_000;
        reading.appCpuMillis = 1_234;
        return reading;
    }

    @Test
    public void retainsExactCountersAndExplicitUnits() throws Exception {
        DebugPowerSnapshot.Reading reading = reading();
        reading.energyNanowattHours = 18_123_456_789L;
        reading.chargeMicroampHours = 4_000_000;
        reading.currentMicroamps = -12_345;
        reading.averageCurrentMicroamps = -10_000;
        reading.capacityPercent = 81;
        reading.batteryStatus = 3;
        reading.pluggedSources = 0;
        reading.voltageMillivolts = 4_120;
        reading.temperatureTenthsCelsius = 264;
        reading.interactive = false;
        reading.deviceIdle = true;
        JSONObject result = DebugPowerSnapshot.from(reading);
        assertEquals(1, result.getInt("schema_version"));
        assertEquals("device_battery", result.getString("scope"));
        assertEquals(18_123_456_789L, result.getLong("energy_remaining_nwh"));
        assertEquals(4_000_000, result.getLong("charge_remaining_uah"));
        assertEquals(-12_345, result.getInt("current_now_ua"));
        assertEquals(-10_000, result.getInt("current_average_ua"));
        assertEquals(81, result.getInt("capacity_percent"));
        assertEquals(3, result.getInt("battery_status"));
        assertEquals(0, result.getInt("plugged_sources"));
        assertEquals(4_120, result.getInt("voltage_mv"));
        assertEquals(264, result.getInt("temperature_tenths_c"));
        assertFalse(result.getBoolean("screen_interactive"));
        assertTrue(result.getBoolean("device_idle"));
        assertEquals(100_000, result.getLong("started_elapsed_realtime_ms"));
        assertEquals(100_010, result.getLong("finished_elapsed_realtime_ms"));
        assertEquals(40_000, result.getLong("uptime_ms"));
        assertEquals(1_234, result.getLong("app_cpu_time_ms"));
    }

    @Test
    public void unsupportedReadingsAreNullNotZero() throws Exception {
        JSONObject result = DebugPowerSnapshot.from(reading());
        for (String key : new String[] {
            "energy_remaining_nwh", "charge_remaining_uah", "current_now_ua",
            "current_average_ua", "capacity_percent", "battery_status", "plugged_sources",
            "voltage_mv", "temperature_tenths_c", "screen_interactive", "device_idle"
        }) {
            assertTrue(key, result.has(key));
            assertTrue(key, result.isNull(key));
        }
    }

    @Test
    public void preservesZeroAndNegativeTemperatureAndSignedCurrent() throws Exception {
        DebugPowerSnapshot.Reading reading = reading();
        reading.energyNanowattHours = 0;
        reading.chargeMicroampHours = 0;
        reading.currentMicroamps = 0;
        reading.averageCurrentMicroamps = 10;
        reading.capacityPercent = 0;
        reading.temperatureTenthsCelsius = -15;
        JSONObject result = DebugPowerSnapshot.from(reading);
        assertEquals(0, result.getLong("energy_remaining_nwh"));
        assertEquals(0, result.getLong("charge_remaining_uah"));
        assertEquals(0, result.getInt("current_now_ua"));
        assertEquals(10, result.getInt("current_average_ua"));
        assertEquals(0, result.getInt("capacity_percent"));
        assertEquals(-15, result.getInt("temperature_tenths_c"));
    }

    @Test
    public void invalidCapacityAndStatusAreUnavailable() throws Exception {
        DebugPowerSnapshot.Reading reading = reading();
        reading.energyNanowattHours = -1;
        reading.chargeMicroampHours = -1;
        reading.capacityPercent = 101;
        reading.batteryStatus = 0;
        JSONObject result = DebugPowerSnapshot.from(reading);
        assertTrue(result.isNull("energy_remaining_nwh"));
        assertTrue(result.isNull("charge_remaining_uah"));
        assertTrue(result.isNull("capacity_percent"));
        assertTrue(result.isNull("battery_status"));
    }

    @Test
    public void outputHasOnlyTheDocumentedNumericAndPowerFields() throws Exception {
        JSONObject result = DebugPowerSnapshot.from(reading());
        assertEquals(17, result.length());
        assertFalse(result.has("peer_id"));
        assertFalse(result.has("hostname"));
        assertFalse(result.has("code"));
        assertFalse(result.has("app_energy_nwh"));
        assertFalse(result.has("estimated_energy_nwh"));
    }

    @Test
    public void absentBatteryServicesDoNotStartVpnOrRegisterLiveReceivers() throws Exception {
        ContextWrapper context = new ContextWrapper(null) {
            @Override
            public Intent registerReceiver(BroadcastReceiver receiver, IntentFilter filter) {
                assertNull("endpoint must only read a sticky snapshot", receiver);
                return null;
            }

            @Override
            public ComponentName startService(Intent intent) {
                throw new AssertionError("power capture must not start a service");
            }

            @Override
            public ComponentName startForegroundService(Intent intent) {
                throw new AssertionError("power capture must not start a foreground service");
            }
        };
        JSONObject result = DebugPowerSnapshot.capture(context);
        assertEquals("device_battery", result.getString("scope"));
        assertTrue(result.isNull("energy_remaining_nwh"));
        assertTrue(result.isNull("plugged_sources"));
    }

    @Test(expected = JSONException.class)
    public void rejectsBackwardsObservation() throws Exception {
        DebugPowerSnapshot.Reading reading = reading();
        reading.finishedElapsedMillis = reading.startedElapsedMillis - 1;
        DebugPowerSnapshot.from(reading);
    }

    @Test(expected = JSONException.class)
    public void rejectsUptimeBeyondElapsedTime() throws Exception {
        DebugPowerSnapshot.Reading reading = reading();
        reading.uptimeMillis = reading.finishedElapsedMillis + 1;
        DebugPowerSnapshot.from(reading);
    }

    @Test(expected = JSONException.class)
    public void rejectsNegativeCpuTime() throws Exception {
        DebugPowerSnapshot.Reading reading = reading();
        reading.appCpuMillis = -1;
        DebugPowerSnapshot.from(reading);
    }
}
