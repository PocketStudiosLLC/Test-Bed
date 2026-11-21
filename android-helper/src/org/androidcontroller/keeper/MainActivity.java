package org.androidcontroller.keeper;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.PowerManager;
import android.provider.Settings;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.Switch;
import android.widget.TextView;

/** Status and the few switches; everything is built in code (no resources). Always dark. */
public final class MainActivity extends Activity {
    // The PC app's colours (libadwaita, dark).
    private static final int BG = Ui.WINDOW_BG;
    private static final int CARD = Ui.CARD_BG;
    private static final int TEXT = Ui.FG;
    private static final int DIM = Ui.DIM;
    private static final int GOOD = Ui.SUCCESS;
    private static final int BAD = Ui.WARNING;

    private final Handler handler = new Handler(Looper.getMainLooper());
    private TextView wd;
    private TextView wifi;
    private TextView perm;
    private TextView batteryState;
    private TextView setup;
    private Switch keep;
    private Switch watchdog;
    private Button battery;
    private Switch direct;
    private TextView directStatus;
    private TextView pairView;
    private Button pair;
    private Button touch;
    private Button capture;
    private Button storage;
    private Button forget;
    private Button monitor;
    private TextView monitorStatus;
    private TextView mirrorLabel;
    private LinearLayout mirrorRow;
    /** The PC screens the mirror buttons were made for. */
    private String mirrorShown = "";
    private RemotePcs remotePcs;
    private final Runnable refresh = new Runnable() {
        @Override
        public void run() {
            render();
            handler.postDelayed(this, 1_000);
        }
    };

    @Override
    protected void onCreate(Bundle saved) {
        setTheme(android.R.style.Theme_DeviceDefault_NoActionBar);
        super.onCreate(saved);
        getWindow().setStatusBarColor(BG);
        getWindow().setNavigationBarColor(BG);
        int pad = dp(16);
        LinearLayout col = new LinearLayout(this);
        col.setOrientation(LinearLayout.VERTICAL);
        col.setPadding(pad, pad, pad, pad);

        TextView title = text("Test Bed", 20, TEXT);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        col.addView(title);

        remotePcs = new RemotePcs(this, card(section(col, "Remote PCs")));

        LinearLayout wireless = section(col, "Wireless debugging");
        LinearLayout status = card(wireless);
        wd = row(status, "Wireless debugging");
        wifi = row(status, "Wi-Fi");
        perm = row(status, "Permission");
        batteryState = row(status, "Battery");
        setup = text("", 13, DIM);
        setup.setTextIsSelectable(true);
        setup.setPadding(0, dp(8), 0, 0);
        status.addView(setup);

        LinearLayout buttons = new LinearLayout(this);
        Button now = button("Turn on now", v -> {
            Keeper.turnOn(this);
            render();
        });
        Button dev = button("Dev options", v -> startActivity(new Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS)));
        Ui.style(now, Ui.Kind.SUGGESTED);
        LinearLayout.LayoutParams first = weighted();
        first.setMarginEnd(dp(8));
        buttons.addView(now, first);
        buttons.addView(dev, weighted());
        buttons.setPadding(0, 0, 0, dp(8));
        wireless.addView(buttons);

        battery = button("Allow running in background", v -> {
            Intent i = new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:" + getPackageName()));
            try {
                startActivity(i);
            } catch (RuntimeException e) {
                startActivity(new Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            }
        });
        Ui.style(battery, Ui.Kind.SUGGESTED);
        wireless.addView(battery);

        LinearLayout opts = card(wireless);
        keep = toggle(opts, "Keep wireless debugging on");
        keep.setOnCheckedChangeListener((b, on) -> {
            if (on != Keeper.active(this)) Keeper.setActive(this, on);
        });
        watchdog = toggle(opts, "Restart stuck Wi-Fi");
        watchdog.setOnCheckedChangeListener((b, on) -> Keeper.setWatchdog(this, on));
        Switch locator = toggle(opts, "Let the PC find this phone");
        locator.setChecked(Keeper.locatorEnabled(this));
        locator.setOnCheckedChangeListener((b, on) -> Keeper.setLocatorEnabled(this, on));

        addMonitorSection(col);
        addDirectSection(col);

        ScrollView scroll = new ScrollView(this);
        scroll.setBackgroundColor(BG);
        scroll.addView(col);
        setContentView(scroll);

        if (Keeper.isAtLeast(33) && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
        if (Keeper.active(this)) Keeper.start(this);
        if (Direct.enabled(this)) Direct.start(this);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Monitor.requestOutputs();
        handler.post(refresh);
        remotePcs.resume();
    }

    @Override
    protected void onPause() {
        handler.removeCallbacks(refresh);
        remotePcs.pause();
        super.onPause();
    }

    private void render() {
        monitor.setText(Monitor.active() ? "Exit monitor" : "Use as PC monitor");
        monitorStatus.setText(Monitor.statusLine());
        renderMirror();
        boolean granted = Keeper.canWriteSettings(this);
        keep.setChecked(Keeper.active(this));
        watchdog.setChecked(Keeper.watchdog(this));
        watchdog.setEnabled(Keeper.active(this));
        if (Keeper.hasWirelessDebugging()) {
            boolean on = Keeper.wirelessDebuggingOn(this);
            value(wd, on ? "on" : "off", on);
        } else {
            value(wd, "needs Android 11", false);
        }
        boolean onWifi = Keeper.onWifi(this);
        value(wifi, onWifi ? "connected" : "not connected", onWifi);
        value(perm, granted ? "granted" : "missing", granted);
        PowerManager pm = getSystemService(PowerManager.class);
        boolean unrestricted = pm != null && pm.isIgnoringBatteryOptimizations(getPackageName());
        value(batteryState, unrestricted ? "unrestricted" : "restricted", unrestricted);
        battery.setVisibility(unrestricted ? View.GONE : View.VISIBLE);
        String hint = "";
        if (!Keeper.hasWirelessDebugging()) {
            hint = "Use Enable Wi-Fi ADB in Test Bed (USB once), or PC connection without ADB below.";
        } else if (!granted) {
            hint = "Connect the phone in Test Bed, or run on the PC:\n" + Keeper.GRANT_COMMAND;
        }
        setup.setText(hint);
        setup.setVisibility(hint.isEmpty() ? View.GONE : View.VISIBLE);
        renderDirect();
    }

    private void addMonitorSection(LinearLayout col) {
        LinearLayout c = card(section(col, "PC monitor"));
        monitor = button("", v -> {
            Monitor.startServer(this);
            Monitor.userToggle(this, !Monitor.active());
            render();
        });
        Ui.style(monitor, Ui.Kind.SUGGESTED);
        c.addView(monitor);
        monitorStatus = text("", 13, DIM);
        c.addView(monitorStatus);
        mirrorLabel = text("Mirror display", 14, TEXT);
        mirrorLabel.setPadding(0, dp(12), 0, dp(4));
        c.addView(mirrorLabel);
        mirrorRow = new LinearLayout(this);
        c.addView(mirrorRow);
    }

    /** One button per PC screen, numbered left to right as the PC reports them. */
    private void renderMirror() {
        String[] outputs = Monitor.outputs();
        String key = android.text.TextUtils.join(",", outputs);
        if (!key.equals(mirrorShown)) {
            mirrorShown = key;
            mirrorRow.removeAllViews();
            for (int i = 0; i < outputs.length; i++) {
                String name = outputs[i];
                Button b = button(String.valueOf(i + 1), v -> {
                    Monitor.startServer(this);
                    Monitor.userMirror(this, name);
                    render();
                });
                LinearLayout.LayoutParams lp = weighted();
                lp.setMarginEnd(i + 1 < outputs.length ? dp(6) : 0);
                mirrorRow.addView(b, lp);
            }
        }
        int vis = outputs.length > 0 ? View.VISIBLE : View.GONE;
        mirrorLabel.setVisibility(vis);
        mirrorRow.setVisibility(vis);
    }

    /** "PC connection without ADB": for phones with no wireless debugging and no USB cable. */
    private void addDirectSection(LinearLayout col) {
        LinearLayout c = card(section(col, "Without ADB"));
        direct = toggle(c, "Allow PC connection");
        direct.setOnCheckedChangeListener((b, on) -> {
            if (on == Direct.enabled(this)) return;
            Direct.setEnabled(this, on);
            if (on && checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(new String[] {Manifest.permission.WRITE_EXTERNAL_STORAGE}, 2);
            }
            render();
        });
        c.addView(text("For Android 10 and older without a USB cable", 13, DIM));

        directStatus = text("", 14, TEXT);
        directStatus.setPadding(0, dp(8), 0, dp(4));
        directStatus.setTextIsSelectable(true);
        c.addView(directStatus);

        pairView = text("", 15, TEXT);
        pairView.setTextIsSelectable(true);
        pairView.setPadding(0, 0, 0, dp(4));
        c.addView(pairView);

        pair = button("", v -> {
            if (Direct.pairCode() != null) {
                Direct.closePairing();
            } else {
                Direct.openPairing();
            }
            render();
        });
        c.addView(pair);
        touch = button("Turn on touch control", v -> startActivity(new Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)));
        c.addView(touch);
        capture = button("Allow screen capture", v -> Projection.ask(this));
        c.addView(capture);
        storage = button("Allow receiving files",
                v -> requestPermissions(new String[] {Manifest.permission.WRITE_EXTERNAL_STORAGE}, 2));
        c.addView(storage);
        forget = button("Forget paired PCs", v -> {
            Direct.forgetPcs(this);
            render();
        });
        Ui.style(forget, Ui.Kind.DESTRUCTIVE);
        c.addView(forget);
    }

    private void renderDirect() {
        boolean on = Direct.enabled(this);
        direct.setChecked(on);
        String ip = Direct.wifiAddress(this);
        boolean input = InputService.enabled(this);
        boolean files = checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED;
        StringBuilder s = new StringBuilder();
        if (on) {
            s.append(DirectService.statusLine()).append(" · ").append(ip == null ? "no Wi-Fi" : ip + ":" + Direct.PORT);
            if (!input) s.append("\nTouch off: the PC can only watch");
            int n = Direct.pcCount(this);
            s.append("\nPaired: ").append(n == 0 ? "none" : Direct.pcNames(this));
        }
        directStatus.setText(s);
        directStatus.setVisibility(on ? View.VISIBLE : View.GONE);
        String code = Direct.pairCode();
        if (on && code != null) {
            pairView.setText("Code  " + code.substring(0, 3) + " " + code.substring(3) + "   (" + Direct.pairSecondsLeft() + " s)\n"
                    + "Test Bed › Add Device › No ADB");
            pairView.setTextSize(17);
        } else if (on && Direct.lastPaired != null) {
            pairView.setText("Paired with " + Direct.lastPaired);
            pairView.setTextSize(15);
        } else {
            pairView.setText("");
        }
        pairView.setVisibility(pairView.getText().length() > 0 ? View.VISIBLE : View.GONE);
        pair.setText(code != null ? "Stop pairing" : "Pair with PC");
        for (View v : new View[] {pair, forget}) v.setVisibility(on ? View.VISIBLE : View.GONE);
        touch.setVisibility(on && !input ? View.VISIBLE : View.GONE);
        capture.setVisibility(on && Projection.get() == null ? View.VISIBLE : View.GONE);
        storage.setVisibility(on && !files ? View.VISIBLE : View.GONE);
        forget.setEnabled(Direct.pcCount(this) > 0);
    }

    // ---- small view helpers ---------------------------------------------

    private TextView text(String s, int sp, int color) {
        TextView t = new TextView(this);
        t.setText(s);
        t.setTextSize(sp);
        t.setTextColor(color);
        return t;
    }

    /**
     * A dropdown: a heading that opens and closes what is under it. Each
     * remembers whether it was open; all start open.
     */
    private LinearLayout section(LinearLayout col, String title) {
        LinearLayout body = new LinearLayout(this);
        body.setOrientation(LinearLayout.VERTICAL);
        TextView h = heading(title);
        Dropdown.attach(this, h, body, title);
        col.addView(h);
        col.addView(body);
        return body;
    }

    private TextView heading(String s) {
        TextView t = text(s, 15, TEXT);
        t.setTypeface(Typeface.DEFAULT_BOLD);
        t.setPadding(dp(4), dp(16), 0, dp(6));
        return t;
    }

    /** A rounded panel appended to `parent`; returns it to add rows to. */
    private LinearLayout card(LinearLayout parent) {
        LinearLayout c = new LinearLayout(this);
        c.setOrientation(LinearLayout.VERTICAL);
        int p = dp(12);
        c.setPadding(p, p, p, p);
        GradientDrawable bg = new GradientDrawable();
        bg.setColor(CARD);
        bg.setCornerRadius(dp(12));
        c.setBackground(bg);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = dp(8);
        parent.addView(c, lp);
        return c;
    }

    /** A "label ... value" line; returns the value view. */
    private TextView row(LinearLayout parent, String label) {
        LinearLayout r = new LinearLayout(this);
        r.setPadding(0, dp(3), 0, dp(3));
        TextView l = text(label, 14, DIM);
        r.addView(l, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        TextView v = text("", 14, TEXT);
        v.setGravity(Gravity.END);
        r.addView(v);
        parent.addView(r);
        return v;
    }

    private void value(TextView v, String s, boolean good) {
        v.setText(s);
        v.setTextColor(good ? GOOD : BAD);
    }

    private Switch toggle(LinearLayout parent, String label) {
        Switch s = new Switch(this);
        s.setText(label);
        s.setTextSize(15);
        s.setTextColor(TEXT);
        s.setPadding(0, dp(6), 0, dp(6));
        Ui.style(s);
        parent.addView(s);
        return s;
    }

    /** A round button, as on the PC; a full-width one in a column, with a gap above. */
    private Button button(String label, View.OnClickListener onClick) {
        Button b = Ui.button(this, label, Ui.Kind.NORMAL);
        b.setPadding(dp(16), dp(10), dp(16), dp(10));
        b.setOnClickListener(onClick);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT);
        lp.topMargin = dp(8);
        b.setLayoutParams(lp);
        return b;
    }

    private static LinearLayout.LayoutParams weighted() {
        return new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1);
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }
}
