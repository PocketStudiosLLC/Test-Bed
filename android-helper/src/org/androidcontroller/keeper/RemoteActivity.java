package org.androidcontroller.keeper;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.graphics.Color;
import android.graphics.Rect;
import android.graphics.drawable.GradientDrawable;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.media.MediaCodec;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.util.Log;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.Window;
import android.view.WindowManager;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.HashSet;
import java.util.Set;

/**
 * Viewing and using a PC that shares with Test Bed. The picture is the PC's
 * screen: tap to click, hold for a right click, drag to drag, two fingers to
 * scroll (a two-finger tap right-clicks too). A bar along the top, like the
 * one on Test Bed for PCs, shows the PC's name, its monitors and how the
 * stream is doing, and has: what a tap does, the keyboard (which types on the
 * PC, with a row of PC keys above it), sending and receiving files, a
 * screenshot, Task Manager (Windows), sound, reconnecting, hiding the bar,
 * and disconnecting. The clipboard is shared both ways while it is open.
 */
public final class RemoteActivity extends Activity {
    static final String EXTRA_FINGERPRINT = "fingerprint";
    static final String EXTRA_NAME = "name";

    /** The top bar's height, in dp. */
    private static final int TOOLBAR_DP = 48;
    private static final int REQ_SEND = 1, REQ_STORAGE = 2;
    /** The bar's background: the PC app's header bar, nearly opaque. */
    private static final int BAR_BG = 0xEE000000 | (Ui.HEADER_BG & 0xFFFFFF);
    private static final String PREFS = "remote", PREF_MUTED = "muted";
    private static final long MIN_MOVE_GAP_MS = 8;
    private static final long TWO_FINGER_TAP_MS = 300;
    private static final long BACK_AGAIN_MS = 2_000;

    private final Handler main = new Handler(Looper.getMainLooper());
    private String fingerprint;
    private String pcName;
    private FrameLayout root;
    private SurfaceView video;
    private TextView statusView;
    private TextView noticeView;
    private Typing typing;
    private HorizontalScrollView strip;
    private LinearLayout bar, monitorsRow;
    private TextView titleView, statsView, transferView;
    private Button clickButton, keyboardButton, sendButton, receiveButton, screenshotButton, taskManagerButton,
            soundButton, reconnectButton, showBarButton;
    /** The bar is hidden for a bigger picture; a small button brings it back. */
    private boolean barHidden;
    /** Connected now (input and files go through), and the connection gave up for good. */
    private boolean connected, fatal;
    private String hostOs = "";
    /** Frames since the stats were last shown (session thread), and the latest round trip. */
    private volatile int frames;
    private volatile double rttMs = -1;
    private long statsAt;
    /** Files on their way, each way, and the copies made to send them. */
    private int sending, receiving;
    private RemoteFiles browser;
    private final java.util.concurrent.ExecutorService work = java.util.concurrent.Executors.newSingleThreadExecutor();
    /** A tap right-clicks instead of left-clicking. */
    private boolean rightTap;
    /** The PC's monitors, numbered in reading order, and the one streamed. */
    private final java.util.List<String> monitors = new java.util.ArrayList<>();
    private String monitor = "";
    private boolean imeOpen;
    /** The session runs over the internet (to a PC paired at home). */
    private boolean routeAway;
    private final Set<Integer> latched = new HashSet<>();
    private final java.util.Map<Integer, Button> modButtons = new java.util.HashMap<>();
    private boolean warnedChar;
    private long backAt;

    private volatile long session;
    /** The picture is showing: the status line is cleared. */
    private boolean showing;
    private Thread netThread;
    private final Decoder decoder = new Decoder();
    private final AudioOut audio = new AudioOut();

    private int videoW, videoH;
    private ClipboardManager clipboard;
    /** The last clipboard text either way, so nothing echoes back. */
    private volatile String lastClip;
    /** When the phone's clipboard last went to the PC. */
    private long clipSentAt;
    private final ClipboardManager.OnPrimaryClipChangedListener clipChanged = this::sendClipboard;
    /** How much of the window the phone's keyboard covers. */
    private int imeHeight;

    @Override
    protected void onCreate(Bundle saved) {
        super.onCreate(saved);
        fingerprint = getIntent().getStringExtra(EXTRA_FINGERPRINT);
        pcName = getIntent().getStringExtra(EXTRA_NAME);
        if (fingerprint == null || !Remote.available(this)) {
            finish();
            return;
        }
        requestWindowFeature(Window.FEATURE_NO_TITLE);
        Window w = getWindow();
        w.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON | WindowManager.LayoutParams.FLAG_FULLSCREEN);
        // The keyboard covers the picture's bottom instead of squeezing it.
        w.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING);
        if (Keeper.isAtLeast(28)) {
            WindowManager.LayoutParams lp = w.getAttributes();
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            w.setAttributes(lp);
        }
        setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE);

        root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);
        root.addOnLayoutChangeListener((v, l, t, r, b, ol, ot, or, ob) -> {
            if (r - l != or - ol || b - t != ob - ot) v.post(this::fitVideo);
        });
        if (Keeper.isAtLeast(30)) {
            // Full screen, the visible frame doesn't shrink for the keyboard; its inset does.
            root.setOnApplyWindowInsetsListener((v, insets) -> {
                imeHeight = insets.getInsets(android.view.WindowInsets.Type.ime()).bottom;
                placeStrip();
                return insets;
            });
        } else {
            root.getViewTreeObserver().addOnGlobalLayoutListener(() -> {
                Rect r = new Rect();
                root.getWindowVisibleDisplayFrame(r);
                imeHeight = root.getRootView().getHeight() - r.bottom;
                placeStrip();
            });
        }

        typing = new Typing(this);
        root.addView(typing, new FrameLayout.LayoutParams(1, 1));

        video = new SurfaceView(this);
        video.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder h) {
                decoder.setSurface(h.getSurface());
            }

            @Override
            public void surfaceChanged(SurfaceHolder h, int format, int width, int height) {}

            @Override
            public void surfaceDestroyed(SurfaceHolder h) {
                decoder.setSurface(null);
            }
        });
        FrameLayout.LayoutParams vlp = new FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.MATCH_PARENT, Gravity.TOP | Gravity.CENTER_HORIZONTAL);
        vlp.topMargin = dp(TOOLBAR_DP);
        root.addView(video, vlp);

        statusView = new TextView(this);
        statusView.setTextColor(Color.WHITE);
        statusView.setTextSize(18);
        statusView.setGravity(Gravity.CENTER);
        statusView.setPadding(dp(24), 0, dp(24), 0);
        root.addView(statusView, new FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT,
                FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.CENTER));

        noticeView = new TextView(this);
        noticeView.setTextColor(Color.WHITE);
        noticeView.setTextSize(14);
        noticeView.setPadding(dp(12), dp(6), dp(12), dp(6));
        noticeView.setBackground(rounded(0xE6000000 | (Ui.WINDOW_BG & 0xFFFFFF), dp(12)));
        noticeView.setVisibility(View.GONE);
        FrameLayout.LayoutParams nlp = new FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT,
                FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.TOP | Gravity.CENTER_HORIZONTAL);
        nlp.topMargin = dp(TOOLBAR_DP + 12);
        root.addView(noticeView, nlp);

        bar = makeToolbar();
        root.addView(bar, new FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, dp(TOOLBAR_DP), Gravity.TOP));
        showBarButton = Ui.circle(this, "☰", Ui.Kind.NORMAL);
        showBarButton.setContentDescription("Show the toolbar");
        showBarButton.setBackground(Ui.pill(0x99000000));
        showBarButton.setVisibility(View.GONE);
        showBarButton.setOnClickListener(v -> setBarHidden(false));
        FrameLayout.LayoutParams slp = new FrameLayout.LayoutParams(dp(44), dp(36), Gravity.TOP | Gravity.END);
        slp.topMargin = dp(6);
        slp.setMarginEnd(dp(6));
        root.addView(showBarButton, slp);

        strip = makeStrip();
        strip.setVisibility(View.GONE);
        root.addView(strip, new FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT,
                FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM));

        setContentView(root);
        clipboard = getSystemService(ClipboardManager.class);
        if (clipboard != null) clipboard.addPrimaryClipChangedListener(clipChanged);
        setStatus("Connecting to " + pcName + "…");
        cleanTransfers();
        start();
        main.post(stats);
    }

    @Override
    protected void onResume() {
        super.onResume();
        hideSystemBars();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            hideSystemBars();
            // Something copied in another app: Android lets it be read now.
            main.post(this::sendClipboard);
        } else if (session != 0) {
            Remote.releaseAll(session);
        }
    }

    @Override
    protected void onDestroy() {
        if (clipboard != null) clipboard.removePrimaryClipChangedListener(clipChanged);
        main.removeCallbacks(stats);
        if (browser != null) browser.dismiss();
        stop();
        work.execute(this::cleanTransfers);
        work.shutdown();
        super.onDestroy();
    }

    @Override
    public void onBackPressed() {
        long now = System.currentTimeMillis();
        if (now - backAt < BACK_AGAIN_MS) {
            finish();
            return;
        }
        backAt = now;
        Toast.makeText(this, "Press back again to disconnect", Toast.LENGTH_SHORT).show();
    }

    @SuppressWarnings("deprecation")
    private void hideSystemBars() {
        getWindow().getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_FULLSCREEN
                | View.SYSTEM_UI_FLAG_LAYOUT_STABLE | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN);
    }

    // ---- the session ------------------------------------------------------------------------

    private void start() {
        String name = Remote.deviceName();
        int maxHeight = Math.min(1080, Math.max(720, getResources().getDisplayMetrics().heightPixels));
        netThread = new Thread(() -> {
            long h = Remote.connect(fingerprint, name, maxHeight);
            if (h == 0) {
                String why = Remote.lastError();
                main.post(() -> setStatus("Can't connect to " + pcName + ": " + why));
                return;
            }
            session = h;
            Remote.watchClipboard(h);
            main.post(this::sendClipboard);
            try {
                while (!Thread.currentThread().isInterrupted()) {
                    byte[] ev = Remote.next(h, 250);
                    if (ev == null || ev.length == 0) continue;
                    switch (ev[0]) {
                        case Remote.EV_VIDEO:
                            if (!showing) {
                                showing = true;
                                main.post(() -> setStatus(""));
                            }
                            frames++;
                            decoder.frame(ev, ev[1] != 0, h);
                            break;
                        case Remote.EV_AUDIO:
                            audio.packet(ev);
                            break;
                        default:
                            if (!onStatus(new String(ev, 1, ev.length - 1, java.nio.charset.StandardCharsets.UTF_8))) return;
                            break;
                    }
                }
            } finally {
                decoder.release();
                audio.release();
            }
        }, "remote-session");
        netThread.start();
    }

    private void stop() {
        long h = session;
        session = 0;
        if (netThread != null) {
            netThread.interrupt();
            try {
                netThread.join(1_000);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            netThread = null;
        }
        if (h != 0) Remote.close(h);
    }

    /** Handles a status event on the session thread; false once the session is over. */
    private boolean onStatus(String json) {
        JSONObject o;
        try {
            o = new JSONObject(json);
        } catch (JSONException e) {
            return true;
        }
        String text = o.optString("text");
        switch (o.optString("t")) {
            case "connecting":
                showing = false;
                main.post(() -> setConnected(false));
                if (o.optInt("attempt") <= 1) main.post(() -> setStatus("Connecting to " + pcName + "…"));
                break;
            case "connected":
                java.util.List<String> names = new java.util.ArrayList<>();
                org.json.JSONArray ms = o.optJSONArray("monitors");
                for (int i = 0; ms != null && i < ms.length(); i++) {
                    JSONObject m = ms.optJSONObject(i);
                    if (m != null) names.add(m.optString("name"));
                }
                String os = o.optString("os");
                main.post(() -> {
                    setStatus("Waiting for the picture…");
                    monitors.clear();
                    monitors.addAll(names);
                    hostOs = os;
                    setConnected(true);
                    renderMonitors();
                });
                break;
            case "clock":
                rttMs = o.optDouble("rtt_ms", -1);
                break;
            case "file_progress":
            case "take_progress": {
                boolean out = o.optString("t").equals("file_progress");
                String name = o.optString("name");
                long done = o.optLong("done"), total = o.optLong("total");
                main.post(() -> progress(out, name, done, total));
                break;
            }
            case "file_sent":
                main.post(() -> {
                    sending = Math.max(0, sending - 1);
                    renderTransfer();
                    notice("Sent " + o.optString("name") + " to " + pcName + ": " + o.optString("path"));
                });
                break;
            case "file_failed":
                main.post(() -> {
                    sending = Math.max(0, sending - 1);
                    renderTransfer();
                    notice("Couldn't send " + o.optString("name") + ": " + text);
                });
                break;
            case "listing":
                main.post(() -> {
                    if (browser != null) browser.listing(o.optLong("id"), o);
                });
                break;
            case "taken":
                saveTaken(o.optString("name"), o.optString("path"));
                break;
            case "take_failed":
                main.post(() -> {
                    receiving = Math.max(0, receiving - 1);
                    renderTransfer();
                    notice("Couldn't receive " + o.optString("name") + ": " + text);
                });
                break;
            case "stream":
                int w = o.optInt("width"), h = o.optInt("height");
                String shown = o.optString("monitor");
                decoder.size(w, h);
                main.post(() -> {
                    videoW = w;
                    videoH = h;
                    monitor = shown;
                    renderMonitors();
                    fitVideo();
                });
                break;
            case "notice":
                main.post(() -> notice(text));
                break;
            case "clipboard":
                main.post(() -> fromPc(text));
                break;
            case "route":
                boolean away = o.optBoolean("internet");
                main.post(() -> {
                    if (away && !routeAway) notice("Connected to " + pcName + " over the internet");
                    routeAway = away;
                });
                break;
            case "linked":
                if (o.optBoolean("new")) {
                    main.post(() -> notice(pcName + " can now be reached from other networks too"));
                }
                break;
            case "link_failed":
                // Said once per PC: it is retried on every connection from home.
                main.post(() -> {
                    android.content.SharedPreferences p = getSharedPreferences("remote", MODE_PRIVATE);
                    String key = "link-hint:" + fingerprint;
                    if (p.getBoolean(key, false)) return;
                    p.edit().putBoolean(key, true).apply();
                    notice(text);
                });
                break;
            case "reconnecting":
                showing = false;
                main.post(() -> {
                    setConnected(false);
                    long s = (o.optLong("retry_ms") + 999) / 1000;
                    setStatus("Reconnecting in " + s + " s…\n" + text);
                });
                break;
            case "disconnected":
                // A reconnect follows by itself.
                showing = false;
                main.post(() -> {
                    setConnected(false);
                    setStatus("Connection lost: " + text);
                });
                break;
            case "fatal":
                main.post(() -> {
                    fatal = true;
                    setConnected(false);
                    setStatus(text.isEmpty() ? "Disconnected" : "Can't connect\n" + text);
                });
                return false;
            case "closed":
                return false;
            default:
                break;
        }
        return true;
    }

    private void setStatus(String text) {
        statusView.setText(text);
        statusView.setVisibility(text.isEmpty() ? View.GONE : View.VISIBLE);
    }

    private final Runnable hideNotice = () -> noticeView.setVisibility(View.GONE);

    private void notice(String text) {
        noticeView.setText(text);
        noticeView.setVisibility(View.VISIBLE);
        main.removeCallbacks(hideNotice);
        main.postDelayed(hideNotice, 4_000);
    }

    /** Keeps the PC screen's shape, in the space below the bar. */
    private void fitVideo() {
        int barH = barHidden ? 0 : dp(TOOLBAR_DP);
        int rw = root.getWidth(), rh = root.getHeight() - barH;
        if (videoW <= 0 || videoH <= 0 || rw <= 0 || rh <= 0) return;
        float scale = Math.min((float) rw / videoW, (float) rh / videoH);
        FrameLayout.LayoutParams lp = (FrameLayout.LayoutParams) video.getLayoutParams();
        int w = Math.round(videoW * scale), h = Math.round(videoH * scale);
        int top = barH + (rh - h) / 2;
        if (lp.width == w && lp.height == h && lp.topMargin == top && lp.bottomMargin == 0) return;
        lp.width = w;
        lp.height = h;
        lp.topMargin = top;
        lp.bottomMargin = 0;
        video.setLayoutParams(lp);
    }

    // ---- the bar -------------------------------------------------------------------------------

    /**
     * As on Test Bed for PCs: the PC's name, its monitors and the stream's
     * numbers, then the actions; Disconnect stays at the end. The actions
     * scroll sideways when the phone is too narrow for all of them.
     */
    private LinearLayout makeToolbar() {
        LinearLayout bar = new LinearLayout(this);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setPadding(dp(10), 0, dp(6), 0);
        bar.setBackgroundColor(BAR_BG);
        // Touches between the buttons don't reach the PC.
        bar.setClickable(true);

        titleView = new TextView(this);
        titleView.setText(pcName);
        titleView.setTextColor(Color.WHITE);
        titleView.setTextSize(14);
        titleView.setTypeface(android.graphics.Typeface.DEFAULT_BOLD);
        titleView.setSingleLine(true);
        titleView.setEllipsize(android.text.TextUtils.TruncateAt.END);
        titleView.setMaxWidth(dp(160));
        bar.addView(titleView);

        monitorsRow = new LinearLayout(this);
        monitorsRow.setGravity(Gravity.CENTER_VERTICAL);
        monitorsRow.setPadding(dp(8), 0, 0, 0);
        bar.addView(monitorsRow);

        statsView = new TextView(this);
        statsView.setTextColor(Ui.DIM);
        statsView.setTextSize(12);
        statsView.setSingleLine(true);
        statsView.setPadding(dp(8), 0, dp(8), 0);
        statsView.setGravity(Gravity.END | Gravity.CENTER_VERTICAL);
        bar.addView(statsView, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f));

        LinearLayout actions = new LinearLayout(this);
        actions.setGravity(Gravity.CENTER_VERTICAL);

        transferView = new TextView(this);
        transferView.setTextColor(Ui.DIM);
        transferView.setTextSize(12);
        transferView.setSingleLine(true);
        transferView.setPadding(0, 0, dp(8), 0);
        transferView.setVisibility(View.GONE);
        actions.addView(transferView);

        clickButton = stripButton("");
        clickButton.setOnClickListener(v -> {
            rightTap = !rightTap;
            renderClick();
            notice(rightTap ? "A tap is now a right click" : "A tap is now a left click");
        });
        actions.addView(clickButton, stripParams());
        renderClick();

        keyboardButton = stripButton("⌨ Keyboard");
        keyboardButton.setOnClickListener(v -> {
            if (imeOpen) typing.hide();
            else typing.show();
        });
        actions.addView(keyboardButton, stripParams());

        sendButton = action(actions, "Send file", "Send files from this phone to the Downloads folder on " + pcName,
                this::chooseFiles);
        receiveButton = action(actions, "Receive files", "Copy files from " + pcName + " to this phone's Downloads",
                this::browseFiles);
        screenshotButton = action(actions, "Screenshot", "Save a screenshot of " + pcName + " to Pictures/Screenshots",
                this::screenshot);
        taskManagerButton = action(actions, "Task Manager", "Open Task Manager on " + pcName + " (Ctrl+Shift+Esc)",
                this::openTaskManager);
        taskManagerButton.setVisibility(View.GONE);

        soundButton = stripButton("");
        soundButton.setOnClickListener(v -> {
            boolean m = !audio.muted;
            audio.muted = m;
            getSharedPreferences(PREFS, MODE_PRIVATE).edit().putBoolean(PREF_MUTED, m).apply();
            renderSound();
        });
        audio.muted = getSharedPreferences(PREFS, MODE_PRIVATE).getBoolean(PREF_MUTED, false);
        actions.addView(soundButton, stripParams());
        renderSound();

        reconnectButton = stripButton("Reconnect");
        reconnectButton.setContentDescription("Reconnect, if the keyboard or mouse stopped working");
        reconnectButton.setOnClickListener(v -> reconnect());
        actions.addView(reconnectButton, stripParams());

        Button hide = stripButton("Hide bar");
        hide.setContentDescription("Hide this bar for a bigger picture");
        hide.setOnClickListener(v -> setBarHidden(true));
        actions.addView(hide, stripParams());

        HorizontalScrollView scroller = new HorizontalScrollView(this);
        scroller.setHorizontalScrollBarEnabled(false);
        scroller.addView(actions);
        bar.addView(scroller, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT));

        Button disconnect = Ui.circle(this, "✕", Ui.Kind.FLAT);
        disconnect.setContentDescription("Disconnect");
        disconnect.setOnClickListener(v -> finish());
        LinearLayout.LayoutParams dlp = stripParams();
        dlp.setMarginStart(dp(4));
        dlp.setMarginEnd(0);
        bar.addView(disconnect, dlp);

        renderMonitors();
        setConnected(false);
        return bar;
    }

    private Button action(LinearLayout row, String label, String description, Runnable onClick) {
        Button b = stripButton(label);
        b.setContentDescription(description);
        b.setOnClickListener(v -> onClick.run());
        b.setOnLongClickListener(v -> {
            notice(description);
            return true;
        });
        row.addView(b, stripParams());
        return b;
    }

    private void renderClick() {
        clickButton.setText(rightTap ? "Tap: Right click" : "Tap: Left click");
        Ui.style(clickButton, rightTap ? Ui.Kind.CHECKED : Ui.Kind.NORMAL);
    }

    private void renderSound() {
        boolean m = audio.muted;
        soundButton.setText(m ? "Sound: Off" : "Sound: On");
        soundButton.setContentDescription(m ? "Sound muted: tap to hear " + pcName : "Mute " + pcName + "'s sound");
        Ui.style(soundButton, m ? Ui.Kind.NORMAL : Ui.Kind.CHECKED);
    }

    /** Buttons that need the PC follow the connection. */
    private void setConnected(boolean on) {
        connected = on;
        for (Button b : new Button[] {sendButton, receiveButton, screenshotButton, taskManagerButton}) {
            if (b == null) continue;
            b.setEnabled(on);
        }
        if (taskManagerButton != null) taskManagerButton.setVisibility("windows".equals(hostOs) ? View.VISIBLE : View.GONE);
        if (reconnectButton != null) {
            reconnectButton.setEnabled(!fatal);
        }
        if (!on) {
            transfersLost();
            if (browser != null) browser.dismiss();
        }
    }

    private void setBarHidden(boolean hidden) {
        barHidden = hidden;
        bar.setVisibility(hidden ? View.GONE : View.VISIBLE);
        showBarButton.setVisibility(hidden ? View.VISIBLE : View.GONE);
        FrameLayout.LayoutParams nlp = (FrameLayout.LayoutParams) noticeView.getLayoutParams();
        nlp.topMargin = dp((hidden ? 0 : TOOLBAR_DP) + 12);
        noticeView.setLayoutParams(nlp);
        fitVideo();
        if (hidden) notice("Tap ☰ at the top right to show the bar again");
    }

    /** "12 ms · 60 fps · 1920×1080", once a second, as on the PC. */
    private final Runnable stats = new Runnable() {
        @Override
        public void run() {
            long now = android.os.SystemClock.elapsedRealtime();
            long dt = statsAt == 0 ? 0 : now - statsAt;
            int n = frames;
            frames = 0;
            statsAt = now;
            if (connected && showing && dt > 0) {
                double rtt = rttMs;
                String lat = rtt >= 0 ? Math.round(rtt) + " ms" : "– ms";
                statsView.setText(lat + " · " + Math.round(n * 1000.0 / dt) + " fps · " + videoW + "×" + videoH);
            } else {
                statsView.setText("");
            }
            main.postDelayed(this, 1_000);
        }
    };

    /** One button per monitor when there are several; the streamed one stands out. */
    private void renderMonitors() {
        if (monitorsRow == null) return;
        monitorsRow.removeAllViews();
        if (monitors.size() < 2) return;
        for (int i = 0; i < monitors.size(); i++) {
            String name = monitors.get(i);
            int number = i + 1;
            Button b = stripButton(String.valueOf(number));
            b.setContentDescription("Monitor " + number + ": " + name);
            Ui.style(b, name.equals(monitor) ? Ui.Kind.SUGGESTED : Ui.Kind.NORMAL);
            b.setOnClickListener(v -> switchMonitor(name));
            b.setOnLongClickListener(v -> {
                notice("Monitor " + number + ": " + name);
                return true;
            });
            monitorsRow.addView(b, stripParams());
        }
    }

    private void switchMonitor(String name) {
        long h = session;
        if (h == 0 || name.equals(monitor)) return;
        notice("Switching to " + name + "…");
        Remote.monitor(h, name);
    }

    /** Ctrl+Shift+Esc on the PC, which opens Windows' Task Manager. */
    private void openTaskManager() {
        long h = session;
        if (h == 0 || !connected) return;
        // Keys latched here would turn the chord into something else.
        Remote.releaseAll(h);
        clearLatched();
        int[] chord = {PcKeys.LEFTCTRL, PcKeys.LEFTSHIFT, PcKeys.ESC};
        for (int code : chord) Remote.key(h, code, true);
        for (int i = chord.length - 1; i >= 0; i--) Remote.key(h, chord[i], false);
    }

    /**
     * Drops the connection and dials again at once, also cutting short a wait
     * between retries. The PC lets go of every key and button and starts
     * input afresh: the fix for a keyboard or mouse that stopped.
     */
    private void reconnect() {
        long h = session;
        if (h == 0 || fatal) return;
        if (connected) Remote.releaseAll(h);
        clearLatched();
        showing = false;
        setConnected(false);
        setStatus("Reconnecting to " + pcName + "…");
        Remote.reconnect(h);
    }

    private void clearLatched() {
        if (latched.isEmpty()) return;
        latched.clear();
        renderMods();
    }

    // ---- files and screenshots -----------------------------------------------------------------

    private File sendDir() {
        return new File(getCacheDir(), "remote-send");
    }

    private File takeDir() {
        return new File(getCacheDir(), "remote-take");
    }

    /** Copies left from an earlier visit. */
    private void cleanTransfers() {
        Saved.delete(sendDir());
        Saved.delete(takeDir());
    }

    private void chooseFiles() {
        if (!connected) return;
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*")
                .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        try {
            startActivityForResult(i, REQ_SEND);
        } catch (android.content.ActivityNotFoundException e) {
            notice("This phone has no file picker");
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != REQ_SEND || result != RESULT_OK || data == null) return;
        java.util.List<android.net.Uri> uris = new java.util.ArrayList<>();
        if (data.getClipData() != null) {
            for (int i = 0; i < data.getClipData().getItemCount(); i++) uris.add(data.getClipData().getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        if (uris.isEmpty()) return;
        sending += uris.size();
        renderTransfer();
        // Test Bed sends files from a path: copy each into this app's cache first.
        work.execute(() -> {
            for (android.net.Uri u : uris) {
                String name = displayName(u);
                try {
                    File dir = new File(sendDir(), Long.toString(System.nanoTime()));
                    if (!dir.mkdirs()) throw new java.io.IOException("can't make " + dir);
                    File f = new File(dir, name);
                    try (java.io.InputStream in = getContentResolver().openInputStream(u);
                            java.io.OutputStream out = new java.io.FileOutputStream(f)) {
                        if (in == null) throw new java.io.IOException("can't open it");
                        Saved.copy(in, out);
                    }
                    long h = session;
                    if (h == 0) throw new java.io.IOException("not connected");
                    Remote.sendFile(h, f.getAbsolutePath());
                } catch (java.io.IOException | RuntimeException e) {
                    main.post(() -> {
                        sending = Math.max(0, sending - 1);
                        renderTransfer();
                        notice("Couldn't send " + name + ": " + e.getMessage());
                    });
                }
            }
        });
    }

    private String displayName(android.net.Uri u) {
        try (android.database.Cursor c = getContentResolver().query(u,
                new String[] {android.provider.OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && c.getString(0) != null) return Saved.safeName(c.getString(0));
        } catch (RuntimeException ignored) {
            // fall back to the path's last part
        }
        return Saved.safeName(u.getLastPathSegment());
    }

    /** Receive files: browse the PC's folders and copy files from it. */
    private void browseFiles() {
        if (!connected || browser != null) return;
        askStorage();
        // Keys latched now would apply to whatever is typed after the dialog.
        clearLatched();
        browser = new RemoteFiles(this, pcName, new RemoteFiles.Ask() {
            @Override
            public void list(long id, String dir, String name) {
                long h = session;
                if (h != 0) Remote.listFiles(h, id, dir, name);
            }

            @Override
            public void take(String dir, java.util.List<String> names) {
                takeFiles(dir, names);
            }

            @Override
            public void closed() {
                browser = null;
                hideSystemBars();
            }
        });
    }

    private void takeFiles(String dir, java.util.List<String> names) {
        long h = session;
        if (h == 0 || !connected) {
            notice("Connect to " + pcName + " to receive files");
            return;
        }
        File dest = takeDir();
        if (!dest.isDirectory() && !dest.mkdirs()) {
            notice("Couldn't make room for the files on this phone");
            return;
        }
        for (String name : names) {
            receiving++;
            Remote.takeFile(h, dir, name, dest.getAbsolutePath());
        }
        renderTransfer();
    }

    /** A file from the PC landed in the cache: into Downloads with it (session thread). */
    private void saveTaken(String name, String path) {
        work.execute(() -> {
            File f = new File(path);
            String msg;
            try {
                msg = "Received " + name + " from " + pcName + ": " + Saved.download(this, f, name);
            } catch (java.io.IOException | RuntimeException e) {
                msg = "Couldn't save " + name + ": " + e.getMessage();
            } finally {
                //noinspection ResultOfMethodCallIgnored
                f.delete();
            }
            String m = msg;
            main.post(() -> {
                receiving = Math.max(0, receiving - 1);
                renderTransfer();
                notice(m);
            });
        });
    }

    private void progress(boolean out, String name, long done, long total) {
        long pct = total > 0 ? done * 100 / total : 100;
        int waiting = (out ? sending : receiving) - 1;
        String more = waiting > 0 ? " (+" + waiting + ")" : "";
        transferView.setText((out ? "Sending " : "Receiving ") + name + " · " + pct + "%" + more);
        transferView.setVisibility(View.VISIBLE);
    }

    private void renderTransfer() {
        if (sending + receiving == 0) {
            transferView.setVisibility(View.GONE);
        } else if (transferView.getVisibility() != View.VISIBLE) {
            transferView.setText(sending > 0 ? "Sending…" : "Receiving…");
            transferView.setVisibility(View.VISIBLE);
        }
    }

    /** The connection dropped: files still queued won't go either way. */
    private void transfersLost() {
        int lost = sending + receiving;
        if (lost == 0) return;
        sending = receiving = 0;
        renderTransfer();
        notice((lost == 1 ? "A file transfer" : lost + " file transfers") + " stopped: the connection to " + pcName
                + " dropped");
    }

    /** Saves the picture shown now as a PNG in Pictures/Screenshots. */
    private void screenshot() {
        if (!connected || !showing || videoW <= 0 || videoH <= 0) {
            notice("No picture from " + pcName + " yet");
            return;
        }
        askStorage();
        android.graphics.Bitmap bmp = android.graphics.Bitmap.createBitmap(videoW, videoH, android.graphics.Bitmap.Config.ARGB_8888);
        try {
            android.view.PixelCopy.request(video, bmp, r -> {
                if (r != android.view.PixelCopy.SUCCESS) {
                    bmp.recycle();
                    notice("Couldn't take the screenshot");
                    return;
                }
                String when = new java.text.SimpleDateFormat("yyyy-MM-dd HH-mm-ss", java.util.Locale.ROOT).format(new java.util.Date());
                String name = Saved.safeName(pcName + " " + when + ".png");
                work.execute(() -> {
                    String msg;
                    try {
                        java.io.ByteArrayOutputStream png = new java.io.ByteArrayOutputStream();
                        bmp.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, png);
                        msg = "Screenshot of " + pcName + " saved: " + Saved.screenshot(this, png.toByteArray(), name);
                    } catch (java.io.IOException | RuntimeException e) {
                        msg = "Couldn't save the screenshot: " + e.getMessage();
                    } finally {
                        bmp.recycle();
                    }
                    String m = msg;
                    main.post(() -> notice(m));
                });
            }, main);
        } catch (IllegalArgumentException e) {
            bmp.recycle();
            notice("Couldn't take the screenshot: " + e.getMessage());
        }
    }

    /** Android 6–9 only lets the app write Downloads and Pictures once allowed. */
    private void askStorage() {
        if (Saved.needsPermission(this)) {
            requestPermissions(new String[] {android.Manifest.permission.WRITE_EXTERNAL_STORAGE}, REQ_STORAGE);
        }
    }

    // ---- touch: the PC's mouse ---------------------------------------------------------------

    private enum Mode { NONE, PENDING, DRAG, SCROLL, DONE }

    private Mode mode = Mode.NONE;
    private float downX, downY, lastAvgX, lastAvgY;
    private float scrollRemX, scrollRemY;
    private long twoFingersAt;
    private boolean scrolled;
    private long lastMoveSent;
    private final Runnable longPress = () -> {
        if (mode == Mode.PENDING) {
            mode = Mode.DONE;
            click(PcKeys.BTN_RIGHT);
        }
    };

    @Override
    public boolean onTouchEvent(MotionEvent e) {
        long h = session;
        int slop = ViewConfiguration.get(this).getScaledTouchSlop();
        switch (e.getActionMasked()) {
            case MotionEvent.ACTION_DOWN:
                downX = e.getX();
                downY = e.getY();
                mode = Mode.PENDING;
                moveTo(e.getX(), e.getY());
                main.postDelayed(longPress, ViewConfiguration.getLongPressTimeout());
                break;
            case MotionEvent.ACTION_POINTER_DOWN:
                main.removeCallbacks(longPress);
                if (mode == Mode.DRAG && h != 0) Remote.button(h, PcKeys.BTN_LEFT, false);
                if (mode == Mode.PENDING || mode == Mode.DRAG) {
                    mode = Mode.SCROLL;
                    twoFingersAt = e.getEventTime();
                    scrolled = false;
                    scrollRemX = scrollRemY = 0;
                    lastAvgX = avg(e, true);
                    lastAvgY = avg(e, false);
                }
                break;
            case MotionEvent.ACTION_MOVE:
                if (mode == Mode.PENDING) {
                    if (Math.hypot(e.getX() - downX, e.getY() - downY) > slop) {
                        main.removeCallbacks(longPress);
                        mode = Mode.DRAG;
                        if (h != 0) Remote.button(h, PcKeys.BTN_LEFT, true);
                    }
                }
                if (mode == Mode.DRAG) {
                    long now = e.getEventTime();
                    if (now - lastMoveSent >= MIN_MOVE_GAP_MS) {
                        lastMoveSent = now;
                        moveTo(e.getX(), e.getY());
                    }
                } else if (mode == Mode.SCROLL && e.getPointerCount() >= 2) {
                    scroll(avg(e, true), avg(e, false), slop);
                }
                break;
            case MotionEvent.ACTION_POINTER_UP:
                if (mode == Mode.SCROLL) {
                    if (!scrolled && e.getEventTime() - twoFingersAt < TWO_FINGER_TAP_MS) click(PcKeys.BTN_RIGHT);
                    mode = Mode.DONE;
                }
                break;
            case MotionEvent.ACTION_UP:
                main.removeCallbacks(longPress);
                if (mode == Mode.PENDING) {
                    click(rightTap ? PcKeys.BTN_RIGHT : PcKeys.BTN_LEFT);
                } else if (mode == Mode.DRAG) {
                    moveTo(e.getX(), e.getY());
                    if (h != 0) Remote.button(h, PcKeys.BTN_LEFT, false);
                }
                mode = Mode.NONE;
                break;
            case MotionEvent.ACTION_CANCEL:
                main.removeCallbacks(longPress);
                if (mode == Mode.DRAG && h != 0) Remote.button(h, PcKeys.BTN_LEFT, false);
                mode = Mode.NONE;
                break;
            default:
                break;
        }
        return true;
    }

    private static float avg(MotionEvent e, boolean x) {
        float s = 0;
        int n = Math.min(2, e.getPointerCount());
        for (int i = 0; i < n; i++) s += x ? e.getX(i) : e.getY(i);
        return s / n;
    }

    /** Fingers moving up scroll the page down, as on the phone itself. */
    private void scroll(float ax, float ay, int slop) {
        long h = session;
        float dx = ax - lastAvgX, dy = ay - lastAvgY;
        lastAvgX = ax;
        lastAvgY = ay;
        if (!scrolled && Math.abs(dx) + Math.abs(dy) < slop / 4f) return;
        scrolled = true;
        float notch = dp(40);
        scrollRemX += -dx * 120 / notch;
        scrollRemY += -dy * 120 / notch;
        int sx = (int) scrollRemX, sy = (int) scrollRemY;
        if ((sx != 0 || sy != 0) && h != 0) {
            scrollRemX -= sx;
            scrollRemY -= sy;
            Remote.scroll(h, sx, sy);
        }
    }

    private void moveTo(float x, float y) {
        long h = session;
        if (h == 0) return;
        int[] at = new int[2];
        video.getLocationInWindow(at);
        float w = Math.max(1, video.getWidth()), vh = Math.max(1, video.getHeight());
        Remote.motion(h, clamp((x - at[0]) / w), clamp((y - at[1]) / vh));
    }

    private void click(int button) {
        long h = session;
        if (h == 0) return;
        Remote.button(h, button, true);
        Remote.button(h, button, false);
    }

    private static float clamp(float v) {
        return Math.max(0f, Math.min(1f, v));
    }

    // ---- the clipboard -----------------------------------------------------------------------

    /** The phone's clipboard to the PC, if it changed. Main thread. */
    private void sendClipboard() {
        long h = session;
        if (h == 0 || clipboard == null) return;
        String text;
        try {
            ClipData d = clipboard.getPrimaryClip();
            if (d == null || d.getItemCount() == 0) return;
            CharSequence t = d.getItemAt(0).coerceToText(this);
            text = t == null ? "" : t.toString();
        } catch (RuntimeException e) {
            return; // not readable now (in the background, Android 10+)
        }
        if (text.isEmpty() || text.equals(lastClip)) return;
        lastClip = text;
        Log.i(Keeper.TAG, "clipboard to the PC (" + text.length() + " characters)");
        clipSentAt = System.currentTimeMillis();
        Remote.setClipboard(h, text);
    }

    /** The PC's clipboard changed: the phone's follows. */
    private void fromPc(String text) {
        if (clipboard == null || text.isEmpty() || text.equals(lastClip)) return;
        lastClip = text;
        Log.i(Keeper.TAG, "clipboard from the PC (" + text.length() + " characters)");
        clipboard.setPrimaryClip(ClipData.newPlainText("Test Bed", text));
        // Android 13+ shows its own "Copied" message.
        if (!Keeper.isAtLeast(33)) notice("Copied from " + pcName);
    }

    /** Ctrl+{@code code} on the PC. */
    private void ctrl(int code) {
        long h = session;
        if (h == 0) return;
        Remote.key(h, PcKeys.LEFTCTRL, true);
        Remote.key(h, code, true);
        Remote.key(h, code, false);
        Remote.key(h, PcKeys.LEFTCTRL, false);
    }

    /** Pastes the phone's clipboard on the PC: sends it first if it is new. */
    private void paste() {
        sendClipboard();
        // Text sent just now travels beside the keys: give it a moment to land.
        long wait = Math.max(0, 200 - (System.currentTimeMillis() - clipSentAt));
        main.postDelayed(() -> ctrl(PcKeys.KEY_V), wait);
    }

    // ---- keyboard: the PC's keys -------------------------------------------------------------

    /** Presses {@code code} with the latched modifiers (and Shift if asked), then lets go of all. */
    void type(int code, boolean shift) {
        long h = session;
        if (h == 0 || code == 0) return;
        Set<Integer> mods = new HashSet<>(latched);
        if (shift) mods.add(PcKeys.LEFTSHIFT);
        for (int m : mods) Remote.key(h, m, true);
        Remote.key(h, code, true);
        Remote.key(h, code, false);
        for (int m : mods) Remote.key(h, m, false);
        if (!latched.isEmpty()) {
            latched.clear();
            main.post(this::renderMods);
        }
    }

    void typeText(CharSequence text) {
        for (int i = 0; i < text.length(); i++) {
            int[] k = PcKeys.forChar(text.charAt(i));
            if (k == null) {
                if (!warnedChar) {
                    warnedChar = true;
                    main.post(() -> Toast.makeText(this, "Only characters on a US keyboard can be typed on the PC",
                            Toast.LENGTH_SHORT).show());
                }
                continue;
            }
            type(k[0], k[1] == 1);
        }
    }

    private HorizontalScrollView makeStrip() {
        LinearLayout row = new LinearLayout(this);
        row.setPadding(dp(4), dp(2), dp(4), dp(2));
        row.setBackgroundColor(BAR_BG);
        for (Object[] k : new Object[][] {{"Copy", (Runnable) () -> ctrl(PcKeys.KEY_C)}, {"Paste", (Runnable) this::paste}}) {
            Button b = stripButton((String) k[0]);
            Runnable r = (Runnable) k[1];
            b.setOnClickListener(v -> r.run());
            Ui.style(b, Ui.Kind.SUGGESTED);
            row.addView(b, stripParams());
        }
        Object[][] keys = {
            {"Esc", PcKeys.ESC}, {"Tab", PcKeys.TAB}, {"Ctrl", -PcKeys.LEFTCTRL}, {"Alt", -PcKeys.LEFTALT},
            {"Shift", -PcKeys.LEFTSHIFT}, {"Win", -PcKeys.LEFTMETA}, {"←", PcKeys.LEFT}, {"↑", PcKeys.UP},
            {"↓", PcKeys.DOWN}, {"→", PcKeys.RIGHT}, {"Home", PcKeys.HOME}, {"End", PcKeys.END},
            {"PgUp", PcKeys.PAGEUP}, {"PgDn", PcKeys.PAGEDOWN}, {"Del", PcKeys.DELETE},
        };
        for (Object[] k : keys) {
            String label = (String) k[0];
            int code = (Integer) k[1];
            Button b = stripButton(label);
            if (code < 0) {
                // Modifiers stay down for the next key.
                int mod = -code;
                modButtons.put(mod, b);
                b.setOnClickListener(v -> {
                    if (!latched.remove(mod)) latched.add(mod);
                    renderMods();
                });
            } else {
                b.setOnClickListener(v -> type(code, false));
            }
            row.addView(b, stripParams());
        }
        HorizontalScrollView s = new HorizontalScrollView(this);
        s.setHorizontalScrollBarEnabled(false);
        s.addView(row);
        return s;
    }

    /** A round button, as on the PC, sized for the bar. */
    private Button stripButton(String label) {
        Button b = Ui.button(this, label, Ui.Kind.NORMAL);
        b.setTextSize(13);
        b.setPadding(dp(12), 0, dp(12), 0);
        return b;
    }

    private LinearLayout.LayoutParams stripParams() {
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, dp(36));
        lp.setMarginEnd(dp(4));
        return lp;
    }

    private void renderMods() {
        for (java.util.Map.Entry<Integer, Button> e : modButtons.entrySet()) {
            Ui.style(e.getValue(), latched.contains(e.getKey()) ? Ui.Kind.CHECKED : Ui.Kind.NORMAL);
        }
    }

    /** Puts the PC keys just above the phone's keyboard while it is open. */
    private void placeStrip() {
        int ime = imeHeight;
        boolean open = ime > dp(120);
        if (open != imeOpen) {
            imeOpen = open;
            if (keyboardButton != null) Ui.style(keyboardButton, open ? Ui.Kind.CHECKED : Ui.Kind.NORMAL);
        }
        FrameLayout.LayoutParams lp = (FrameLayout.LayoutParams) strip.getLayoutParams();
        int margin = open ? ime : 0;
        if (open != (strip.getVisibility() == View.VISIBLE) || lp.bottomMargin != margin) {
            lp.bottomMargin = margin;
            strip.setLayoutParams(lp);
            strip.setVisibility(open ? View.VISIBLE : View.GONE);
            if (!open && !latched.isEmpty()) {
                latched.clear();
                renderMods();
            }
        }
    }

    /** An invisible editor: what the keyboard types goes to the PC, nothing stays here. */
    private static final class Typing extends View {
        Typing(Context c) {
            super(c);
            setFocusable(true);
            setFocusableInTouchMode(true);
        }

        @SuppressWarnings("deprecation")
        void show() {
            requestFocus();
            post(() -> {
                InputMethodManager imm = getContext().getSystemService(InputMethodManager.class);
                if (imm == null) return;
                imm.restartInput(this);
                if (!imm.showSoftInput(this, 0)) imm.toggleSoftInput(InputMethodManager.SHOW_FORCED, 0);
            });
        }

        void hide() {
            InputMethodManager imm = getContext().getSystemService(InputMethodManager.class);
            if (imm != null) imm.hideSoftInputFromWindow(getWindowToken(), 0);
        }

        RemoteActivity activity() {
            return (RemoteActivity) getContext();
        }

        /** Keys that skip the keyboard's text path (a hardware keyboard). */
        @Override
        public boolean onKeyDown(int code, KeyEvent e) {
            if (code == KeyEvent.KEYCODE_BACK) return super.onKeyDown(code, e);
            ToPc.send(activity(), e);
            return true;
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo out) {
            // No suggestions: keyboards then send each letter as it is typed.
            out.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                    | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
            out.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_ACTION_NONE;
            return new ToPc(this, activity());
        }
    }

    /** Turns the keyboard's edits into key presses on the PC. */
    private static final class ToPc extends BaseInputConnection {
        private final RemoteActivity a;
        /** Text the keyboard is still composing; the PC already has it typed. */
        private String composing = "";

        ToPc(View v, RemoteActivity a) {
            super(v, false);
            this.a = a;
        }

        /** Types the difference between what the PC has and {@code now}. */
        private void retype(String now) {
            int same = 0;
            while (same < composing.length() && same < now.length() && composing.charAt(same) == now.charAt(same)) same++;
            for (int i = same; i < composing.length(); i++) a.type(PcKeys.BACKSPACE, false);
            a.typeText(now.substring(same));
            composing = now;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            retype(text.toString());
            return true;
        }

        @Override
        public boolean finishComposingText() {
            composing = "";
            return true;
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            retype(text.toString());
            composing = "";
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int before, int after) {
            for (int i = 0; i < before; i++) a.type(PcKeys.BACKSPACE, false);
            for (int i = 0; i < after; i++) a.type(PcKeys.DELETE, false);
            return true;
        }

        @Override
        public boolean performEditorAction(int action) {
            a.type(PcKeys.ENTER, false);
            return true;
        }

        @Override
        public boolean sendKeyEvent(KeyEvent e) {
            if (e.getAction() == KeyEvent.ACTION_DOWN) {
                composing = "";
                send(a, e);
            }
            return true;
        }

        static void send(RemoteActivity a, KeyEvent e) {
            int code = PcKeys.forKeyCode(e.getKeyCode());
            if (code != 0) {
                a.type(code, e.isShiftPressed());
                return;
            }
            int c = e.getUnicodeChar();
            if (c != 0) a.typeText(new String(Character.toChars(c)));
        }
    }

    // ---- video and sound ---------------------------------------------------------------------

    /** H.264 to the SurfaceView. Runs on the session thread; the surface comes and goes. */
    private static final class Decoder {
        private final Object lock = new Object();
        private Surface surface;
        private boolean surfaceChanged;
        private int w, h;
        private MediaCodec codec;
        private Thread drain;
        private boolean needKey = true;

        void setSurface(Surface s) {
            synchronized (lock) {
                surface = s;
                surfaceChanged = true;
            }
        }

        void size(int width, int height) {
            synchronized (lock) {
                if (width != w || height != h) surfaceChanged = true;
                w = width;
                h = height;
            }
        }

        /** {@code ev} is the event: 2 header bytes, then the access unit. */
        void frame(byte[] ev, boolean key, long session) {
            Surface s;
            synchronized (lock) {
                s = surface;
                if (surfaceChanged) {
                    surfaceChanged = false;
                    close();
                }
            }
            if (s == null || w <= 0 || h <= 0) return;
            if (codec == null) {
                try {
                    MediaFormat f = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h);
                    MediaCodec c = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
                    if (Keeper.isAtLeast(30) && lowLatency(c)) f.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
                    c.configure(f, s, null, 0);
                    c.start();
                    codec = c;
                    drain = startDrain(c);
                    needKey = true;
                    Remote.keyframe(session);
                } catch (Exception e) {
                    Log.w(Keeper.TAG, "remote decoder: " + e);
                    return;
                }
            }
            if (needKey && !key) return;
            needKey = false;
            try {
                int i = codec.dequeueInputBuffer(20_000);
                ByteBuffer b = i >= 0 ? codec.getInputBuffer(i) : null;
                if (b == null) {
                    // A dropped frame breaks the ones after it until a keyframe.
                    needKey = true;
                    Remote.keyframe(session);
                    return;
                }
                b.clear();
                b.put(ev, 2, ev.length - 2);
                codec.queueInputBuffer(i, 0, ev.length - 2, System.nanoTime() / 1000, 0);
            } catch (IllegalStateException e) {
                Log.d(Keeper.TAG, "remote decoder: " + e);
                close();
            }
        }

        private static boolean lowLatency(MediaCodec c) {
            try {
                return c.getCodecInfo().getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
                        .isFeatureSupported(android.media.MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency);
            } catch (RuntimeException e) {
                return false;
            }
        }

        private static Thread startDrain(MediaCodec c) {
            Thread t = new Thread(() -> {
                MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
                try {
                    while (!Thread.currentThread().isInterrupted()) {
                        int i = c.dequeueOutputBuffer(info, 50_000);
                        if (i >= 0) c.releaseOutputBuffer(i, true);
                    }
                } catch (IllegalStateException e) {
                    Log.d(Keeper.TAG, "remote decoder output ended: " + e);
                }
            }, "remote-drain");
            t.start();
            return t;
        }

        private void close() {
            if (drain != null) drain.interrupt();
            if (codec != null) {
                try {
                    if (drain != null) drain.join(500);
                    codec.stop();
                } catch (IllegalStateException e) {
                    Log.d(Keeper.TAG, "stopping remote decoder: " + e);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
                codec.release();
            }
            codec = null;
            drain = null;
        }

        void release() {
            synchronized (lock) {
                close();
            }
        }
    }

    /** The PC's sound: Opus (48 kHz stereo) through Android's decoder to an AudioTrack. */
    private static final class AudioOut {
        private static final int RATE = 48_000;
        private MediaCodec codec;
        private AudioTrack track;
        private Thread drain;
        private boolean failed;
        /** Packets are skipped while muted, so nothing plays. */
        volatile boolean muted;

        void packet(byte[] ev) {
            if (failed || muted) return;
            if (codec == null && !open()) return;
            try {
                int i = codec.dequeueInputBuffer(0);
                if (i < 0) return; // behind: skip a packet rather than lag
                ByteBuffer b = codec.getInputBuffer(i);
                if (b == null) return;
                b.clear();
                b.put(ev, 1, ev.length - 1);
                codec.queueInputBuffer(i, 0, ev.length - 1, System.nanoTime() / 1000, 0);
            } catch (IllegalStateException e) {
                Log.d(Keeper.TAG, "remote audio: " + e);
                release();
                failed = true;
            }
        }

        private boolean open() {
            try {
                MediaFormat f = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, RATE, 2);
                ByteBuffer head = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN);
                head.put("OpusHead".getBytes(java.nio.charset.StandardCharsets.US_ASCII));
                head.put((byte) 1).put((byte) 2).putShort((short) 0).putInt(RATE).putShort((short) 0).put((byte) 0);
                head.flip();
                f.setByteBuffer("csd-0", head);
                f.setByteBuffer("csd-1", nanos(0));
                f.setByteBuffer("csd-2", nanos(80_000_000L));
                MediaCodec c = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS);
                c.configure(f, null, null, 0);
                c.start();
                int min = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT);
                AudioTrack t = new AudioTrack.Builder()
                        .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
                        .setAudioFormat(new AudioFormat.Builder().setSampleRate(RATE)
                                .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                        .setBufferSizeInBytes(Math.max(min, RATE / 10 * 4))
                        .setTransferMode(AudioTrack.MODE_STREAM)
                        .build();
                t.play();
                codec = c;
                track = t;
                drain = new Thread(this::drain, "remote-audio");
                drain.start();
                return true;
            } catch (Exception e) {
                Log.w(Keeper.TAG, "remote audio unavailable: " + e);
                failed = true;
                return false;
            }
        }

        private static ByteBuffer nanos(long v) {
            ByteBuffer b = ByteBuffer.allocate(8).order(ByteOrder.nativeOrder());
            b.putLong(v);
            b.flip();
            return b;
        }

        private void drain() {
            MediaCodec c = codec;
            AudioTrack t = track;
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            byte[] pcm = new byte[0];
            try {
                while (!Thread.currentThread().isInterrupted()) {
                    int i = c.dequeueOutputBuffer(info, 50_000);
                    if (i < 0) continue;
                    ByteBuffer b = c.getOutputBuffer(i);
                    if (b != null && info.size > 0) {
                        if (pcm.length < info.size) pcm = new byte[info.size];
                        b.position(info.offset);
                        b.get(pcm, 0, info.size);
                        t.write(pcm, 0, info.size);
                    }
                    c.releaseOutputBuffer(i, false);
                }
            } catch (IllegalStateException e) {
                Log.d(Keeper.TAG, "remote audio output ended: " + e);
            }
        }

        void release() {
            if (drain != null) {
                drain.interrupt();
                try {
                    drain.join(500);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            }
            if (codec != null) {
                try {
                    codec.stop();
                } catch (IllegalStateException ignored) {
                    // already stopped
                }
                codec.release();
            }
            if (track != null) track.release();
            codec = null;
            track = null;
            drain = null;
        }
    }

    // ---- helpers -----------------------------------------------------------------------------

    private static GradientDrawable rounded(int color, int radius) {
        GradientDrawable d = new GradientDrawable();
        d.setColor(color);
        d.setCornerRadius(radius);
        return d;
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    static void open(Context c, String fingerprint, String name) {
        c.startActivity(new Intent(c, RemoteActivity.class).putExtra(EXTRA_FINGERPRINT, fingerprint).putExtra(EXTRA_NAME, name));
    }
}
