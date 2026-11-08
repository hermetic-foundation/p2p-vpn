package org.hermeticfoundation.p2pvpn;

import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.os.BatteryManager;
import android.os.PowerManager;
import android.os.Process;
import android.os.SystemClock;

import org.json.JSONException;
import org.json.JSONObject;

/** Endpoint-only device battery readings; neither CPU nor current is an energy estimate. */
final class DebugPowerSnapshot {
    private DebugPowerSnapshot() {}

    static JSONObject capture(Context context) throws JSONException {
        Reading reading = new Reading();
        reading.startedElapsedMillis = SystemClock.elapsedRealtime();
        reading.uptimeMillis = SystemClock.uptimeMillis();
        reading.appCpuMillis = Process.getElapsedCpuTime();
        BatteryManager battery = context.getSystemService(BatteryManager.class);
        if (battery != null) {
            reading.energyNanowattHours =
                    battery.getLongProperty(BatteryManager.BATTERY_PROPERTY_ENERGY_COUNTER);
            reading.chargeMicroampHours =
                    battery.getIntProperty(BatteryManager.BATTERY_PROPERTY_CHARGE_COUNTER);
            reading.currentMicroamps =
                    battery.getIntProperty(BatteryManager.BATTERY_PROPERTY_CURRENT_NOW);
            reading.averageCurrentMicroamps =
                    battery.getIntProperty(BatteryManager.BATTERY_PROPERTY_CURRENT_AVERAGE);
            reading.capacityPercent =
                    battery.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY);
        }
        Intent state =
                context.registerReceiver(null, new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
        if (state != null) {
            reading.batteryStatus = state.getIntExtra(BatteryManager.EXTRA_STATUS, -1);
            reading.pluggedSources = state.getIntExtra(BatteryManager.EXTRA_PLUGGED, -1);
            reading.voltageMillivolts = state.getIntExtra(BatteryManager.EXTRA_VOLTAGE, -1);
            reading.temperatureTenthsCelsius =
                    state.getIntExtra(BatteryManager.EXTRA_TEMPERATURE, Integer.MIN_VALUE);
        }
        PowerManager power = context.getSystemService(PowerManager.class);
        if (power != null) {
            reading.interactive = power.isInteractive();
            reading.deviceIdle = power.isDeviceIdleMode();
        }
        reading.finishedElapsedMillis = SystemClock.elapsedRealtime();
        return from(reading);
    }

    static JSONObject from(Reading reading) throws JSONException {
        if (reading.startedElapsedMillis < 0
                || reading.finishedElapsedMillis < reading.startedElapsedMillis
                || reading.uptimeMillis < 0
                || reading.uptimeMillis > reading.finishedElapsedMillis
                || reading.appCpuMillis < 0) {
            throw new JSONException("Invalid power observation clocks");
        }
        JSONObject result = new JSONObject();
        result.put("schema_version", 1);
        result.put("scope", "device_battery");
        result.put("started_elapsed_realtime_ms", reading.startedElapsedMillis);
        result.put("finished_elapsed_realtime_ms", reading.finishedElapsedMillis);
        result.put("uptime_ms", reading.uptimeMillis);
        result.put("app_cpu_time_ms", reading.appCpuMillis);
        result.put("energy_remaining_nwh", nonnegative(reading.energyNanowattHours));
        result.put("charge_remaining_uah", nonnegative(reading.chargeMicroampHours));
        result.put("current_now_ua", integerProperty(reading.currentMicroamps));
        result.put("current_average_ua", integerProperty(reading.averageCurrentMicroamps));
        result.put(
                "capacity_percent",
                reading.capacityPercent <= 100
                        ? nonnegative(reading.capacityPercent) : JSONObject.NULL);
        result.put(
                "battery_status",
                reading.batteryStatus >= 1 && reading.batteryStatus <= 5
                        ? reading.batteryStatus : JSONObject.NULL);
        result.put("plugged_sources", nonnegative(reading.pluggedSources));
        result.put("voltage_mv", nonnegative(reading.voltageMillivolts));
        result.put("temperature_tenths_c", integerProperty(reading.temperatureTenthsCelsius));
        result.put("screen_interactive", nullable(reading.interactive));
        result.put("device_idle", nullable(reading.deviceIdle));
        return result;
    }

    private static Object nonnegative(long value) {
        return value < 0 ? JSONObject.NULL : value;
    }

    private static Object integerProperty(int value) {
        return value == Integer.MIN_VALUE ? JSONObject.NULL : value;
    }

    private static Object nullable(Boolean value) {
        return value == null ? JSONObject.NULL : value;
    }

    static final class Reading {
        long startedElapsedMillis;
        long finishedElapsedMillis;
        long uptimeMillis;
        long appCpuMillis;
        long energyNanowattHours = Long.MIN_VALUE;
        int chargeMicroampHours = Integer.MIN_VALUE;
        int currentMicroamps = Integer.MIN_VALUE;
        int averageCurrentMicroamps = Integer.MIN_VALUE;
        int capacityPercent = Integer.MIN_VALUE;
        int batteryStatus = -1;
        int pluggedSources = -1;
        int voltageMillivolts = -1;
        int temperatureTenthsCelsius = Integer.MIN_VALUE;
        Boolean interactive;
        Boolean deviceIdle;
    }
}
