package org.androidcontroller.keeper;

import android.app.Activity;
import android.app.AlertDialog;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.net.wifi.WifiManager;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.text.TextUtils;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The main screen's "Remote PCs", laid out like Test Bed's PC tab on a PC:
 * <b>Computers</b> (paired PCs on this network, whether each answers, and
 * PCs sharing here that aren't paired yet, with a Pair button) and
 * <b>Computers on other networks</b> (PCs paired with an invite code). Each
 * paired PC has Connect and Forget; each group has a refresh button.
 */
final class RemotePcs {
    // The PC app's colours (libadwaita, dark).
    private static final int TEXT = Ui.FG;
    private static final int DIM = Ui.DIM;
    private static final int GOOD = Ui.SUCCESS;
    private static final int LINE = Ui.LINE;
    private static final long PROBE_EVERY_MS = 10_000;

    private final Activity a;
    private final LinearLayout lanList, netList;
    private final ExecutorService work = Executors.newSingleThreadExecutor();
    private final Handler main = new Handler(Looper.getMainLooper());
    private final boolean available;
    private JSONArray hosts = new JSONArray();
    /** Answers from the last check, or null before the first one. */
    private JSONObject online;
    /** PCs sharing on this network, from the last look. */
    private JSONArray found = new JSONArray();
    private boolean resumed, checking;
    private final Runnable probeLoop = new Runnable() {
        @Override
        public void run() {
            refresh(true);
            main.postDelayed(this, PROBE_EVERY_MS);
        }
    };

    RemotePcs(Activity a, LinearLayout card) {
        this.a = a;
        available = Remote.available(a);
        if (!available) {
            card.addView(text("Needs Android 7 or newer.", 13, DIM));
            lanList = netList = null;
            return;
        }
        lanList = new LinearLayout(a);
        lanList.setOrientation(LinearLayout.VERTICAL);
        card.addView(groupHeader("Computers", "PCs on this network that share themselves with Test Bed", "Look again",
                lanList));
        card.addView(lanList);

        View gap = new View(a);
        card.addView(gap, new LinearLayout.LayoutParams(1, dp(16)));
        netList = new LinearLayout(a);
        netList.setOrientation(LinearLayout.VERTICAL);
        card.addView(groupHeader("Computers on other networks", "PCs paired with an invite code, reached over the internet",
                "Check again", netList));
        card.addView(netList);
        render();
    }

    void resume() {
        if (!available) return;
        resumed = true;
        main.removeCallbacks(probeLoop);
        main.post(probeLoop);
    }

    void pause() {
        resumed = false;
        main.removeCallbacks(probeLoop);
    }

    /** Reloads the list; with {@code probe}, also asks each PC whether it is there and looks for new ones. */
    private void refresh(boolean probe) {
        if (probe) {
            if (checking) return;
            checking = true;
        }
        work.execute(() -> {
            try {
                JSONArray h = (JSONArray) Remote.ok(Remote.hosts());
                main.post(() -> {
                    hosts = h;
                    render();
                });
                if (probe && resumed) {
                    JSONObject o = h.length() > 0 ? (JSONObject) Remote.ok(Remote.probe()) : new JSONObject();
                    main.post(() -> {
                        online = o;
                        render();
                    });
                    JSONArray f = discover();
                    main.post(() -> {
                        found = f;
                        render();
                    });
                }
            } catch (Remote.Failed e) {
                main.post(() -> toast(e.getMessage()));
            } finally {
                if (probe) main.post(() -> checking = false);
            }
        });
    }

    /** PCs sharing on this Wi-Fi (off the main thread). */
    private JSONArray discover() {
        WifiManager wifi = a.getApplicationContext().getSystemService(WifiManager.class);
        WifiManager.MulticastLock lock = wifi == null ? null : wifi.createMulticastLock("test-bed-find-pcs");
        try {
            if (lock != null) lock.acquire();
            return (JSONArray) Remote.ok(Remote.discover());
        } catch (Remote.Failed | RuntimeException e) {
            return new JSONArray();
        } finally {
            if (lock != null && lock.isHeld()) lock.release();
        }
    }

    private void render() {
        if (!available) return;
        lanList.removeAllViews();
        netList.removeAllViews();
        List<JSONObject> lan = new ArrayList<>(), net = new ArrayList<>();
        for (int i = 0; i < hosts.length(); i++) {
            JSONObject h = hosts.optJSONObject(i);
            if (h != null) (h.optBoolean("internet") ? net : lan).add(h);
        }
        // Online first, as on the PC.
        java.util.Comparator<JSONObject> up = (x, y) -> Boolean.compare(!isUp(x), !isUp(y));
        lan.sort(up);
        net.sort(up);

        for (JSONObject h : lan) {
            String addr = h.optString("address");
            String fp = h.optString("fingerprint");
            // Paired at home, reachable from anywhere once linked over the internet.
            boolean linked = h.optBoolean("linked"), away = isAway(fp);
            String where = away ? "Another network" : linked ? "This network, or from anywhere" : "This network";
            String sub = (online == null ? "Checking…"
                    : isUp(h) ? "Online · " + Math.round(online.optDouble(fp)) + " ms"
                    : "Offline") + " · " + where + " · " + addr;
            lanList.addView(pcRow(h, sub));
        }
        for (int i = 0; i < found.length(); i++) {
            JSONObject d = found.optJSONObject(i);
            if (d == null) continue;
            String fp = d.optString("fingerprint");
            // Paired and answering: already listed above.
            if (online != null && online.has(fp) && !online.isNull(fp)) continue;
            boolean pairing = d.optBoolean("pairing");
            String addr = d.optString("address"), name = d.optString("name");
            Button pair = Ui.button(a, "Pair", pairing ? Ui.Kind.SUGGESTED : Ui.Kind.NORMAL);
            pair.setOnClickListener(v -> askPin(addr, name));
            lanList.addView(row(name, (pairing ? "Showing a pairing code · " : "Not paired · ") + addr, DIM, pair));
        }
        lanList.addView(actionRow("Pair by address", "For a PC that doesn't show up here", this::askAddress));

        for (JSONObject h : net) {
            String fp = h.optString("fingerprint");
            String sub = online == null ? "Checking…"
                    : isUp(h) ? "Online · " + Math.round(online.optDouble(fp)) + " ms"
                    : "Offline, or access from other networks is off there";
            netList.addView(pcRow(h, sub));
        }
        netList.addView(actionRow("Add with invite code",
                "On the other PC: Allow access from other networks, then Show invite code", this::addInternet));
    }

    /** A PC on this network's list that answered only over the internet. */
    private boolean isAway(String fp) {
        JSONArray away = online == null ? null : online.optJSONArray("away");
        for (int i = 0; away != null && i < away.length(); i++) {
            if (fp.equals(away.optString(i))) return true;
        }
        return false;
    }

    private boolean isUp(JSONObject h) {
        String fp = h.optString("fingerprint");
        return online != null && online.has(fp) && !online.isNull(fp);
    }

    /** A paired PC: Connect, and Forget. */
    private View pcRow(JSONObject h, String sub) {
        String name = h.optString("name"), fp = h.optString("fingerprint");
        boolean up = isUp(h);
        Button connect = Ui.button(a, "Connect", Ui.Kind.SUGGESTED);
        // Offline PCs can still be tried: the viewer keeps dialing until it answers.
        connect.setAlpha(online == null || up ? 1f : 0.5f);
        connect.setOnClickListener(v -> RemoteActivity.open(a, fp, name));
        Button forget = Ui.button(a, "Forget", Ui.Kind.FLAT);
        forget.setContentDescription("Forget " + name);
        forget.setOnClickListener(v -> confirmForget(name, fp));
        return row(name, sub, up ? GOOD : DIM, connect, forget);
    }

    private void confirmForget(String name, String fp) {
        new AlertDialog.Builder(a)
                .setTitle("Forget " + name + "?")
                .setMessage("To control it again, pair it again.")
                .setPositiveButton("Forget", (d, w) -> work.execute(() -> {
                    try {
                        Remote.ok(Remote.forget(fp));
                    } catch (Remote.Failed e) {
                        main.post(() -> toast(e.getMessage()));
                    }
                    refresh(false);
                }))
                .setNegativeButton("Cancel", null)
                .show();
    }

    // ---- pairing -----------------------------------------------------------------------------

    private void askAddress() {
        EditText e = field("192.168.1.20", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        new AlertDialog.Builder(a)
                .setTitle("Pair by address")
                .setMessage("The address of a PC sharing with Test Bed on this network.")
                .setView(padded(e))
                .setPositiveButton("Next", (d, w) -> {
                    String s = e.getText().toString().trim();
                    if (!s.isEmpty()) askPin(s, s);
                })
                .setNegativeButton("Cancel", null)
                .show();
    }

    private void askPin(String address, String name) {
        EditText e = field("6-digit PIN", InputType.TYPE_CLASS_NUMBER);
        new AlertDialog.Builder(a)
                .setTitle("Pair with " + name)
                .setMessage("On the PC, open Test Bed › PC › Pair a computer and type the PIN it shows.")
                .setView(padded(e))
                .setPositiveButton("Pair", (d, w) -> pair(() -> Remote.pairLan(address, e.getText().toString(), Remote.deviceName())))
                .setNegativeButton("Cancel", null)
                .show();
    }

    private void addInternet() {
        EditText e = field("Invite code", InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        e.setSingleLine(false);
        new AlertDialog.Builder(a)
                .setTitle("Add a PC on another network")
                .setMessage("On the PC, turn on Test Bed › PC › Allow access from other networks, then Show invite code, "
                        + "and send yourself the code.")
                .setView(padded(e))
                .setPositiveButton("Add", (d, w) -> pair(() -> Remote.pairInternet(e.getText().toString(), Remote.deviceName())))
                .setNegativeButton("Cancel", null)
                .show();
    }

    private interface Call {
        String run();
    }

    private void pair(Call call) {
        toast("Pairing…");
        work.execute(() -> {
            try {
                JSONObject h = (JSONObject) Remote.ok(call.run());
                main.post(() -> toast("Paired with " + h.optString("name")));
            } catch (Remote.Failed e) {
                main.post(() -> toast("Didn't pair: " + e.getMessage()));
            }
            main.post(() -> refresh(true));
        });
    }

    // ---- small view helpers ------------------------------------------------------------------

    /** A group's title and description, which open and close {@code body}, with a refresh button. */
    private View groupHeader(String title, String description, String refreshLabel, View body) {
        LinearLayout h = new LinearLayout(a);
        h.setGravity(Gravity.CENTER_VERTICAL);
        LinearLayout labels = new LinearLayout(a);
        labels.setOrientation(LinearLayout.VERTICAL);
        TextView t = text(title, 15, TEXT);
        t.setTypeface(Typeface.DEFAULT_BOLD);
        labels.addView(t);
        labels.addView(text(description, 12, DIM));
        h.addView(labels, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        Dropdown.attach(a, labels, t, body, "Remote PCs/" + title);
        Button r = Ui.circle(a, "↻", Ui.Kind.FLAT);
        r.setContentDescription(refreshLabel);
        r.setOnClickListener(v -> {
            toast(refreshLabel.equals("Look again") ? "Looking for PCs…" : "Checking…");
            refresh(true);
        });
        h.addView(r);
        h.setPadding(0, 0, 0, dp(4));
        return h;
    }

    /** Title, subtitle and buttons on the right, with a line above. */
    private View row(String title, String sub, int subColor, View... suffixes) {
        LinearLayout row = new LinearLayout(a);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(0, dp(8), 0, dp(8));
        row.setBackground(topLine());
        LinearLayout labels = new LinearLayout(a);
        labels.setOrientation(LinearLayout.VERTICAL);
        TextView t = text(title, 15, TEXT);
        t.setSingleLine(true);
        t.setEllipsize(TextUtils.TruncateAt.END);
        labels.addView(t);
        labels.addView(text(sub, 12, subColor));
        row.addView(labels, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        for (View s : suffixes) {
            LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, dp(36));
            lp.setMarginStart(dp(6));
            row.addView(s, lp);
        }
        return row;
    }

    /** A row that does something when tapped, with a › at the end. */
    private View actionRow(String title, String sub, Runnable onClick) {
        TextView arrow = text("›", 22, DIM);
        arrow.setPadding(dp(8), 0, dp(4), 0);
        View r = row(title, sub, DIM, arrow);
        r.setClickable(true);
        r.setOnClickListener(v -> onClick.run());
        return r;
    }

    private android.graphics.drawable.Drawable topLine() {
        GradientDrawable line = new GradientDrawable();
        line.setColor(LINE);
        android.graphics.drawable.LayerDrawable d = new android.graphics.drawable.LayerDrawable(
                new android.graphics.drawable.Drawable[] {line});
        d.setLayerHeight(0, Math.max(1, dp(1) / 2));
        d.setLayerGravity(0, Gravity.TOP);
        return d;
    }

    private void toast(String s) {
        Toast.makeText(a, s, Toast.LENGTH_LONG).show();
    }

    private EditText field(String hint, int type) {
        EditText e = new EditText(a);
        e.setHint(hint);
        e.setInputType(type);
        return e;
    }

    private View padded(View v) {
        LinearLayout l = new LinearLayout(a);
        l.setPadding(dp(20), dp(4), dp(20), 0);
        l.addView(v, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        return l;
    }

    private TextView text(String s, int sp, int color) {
        TextView t = new TextView(a);
        t.setText(s);
        t.setTextSize(sp);
        t.setTextColor(color);
        return t;
    }

    private int dp(int v) {
        return Math.round(v * a.getResources().getDisplayMetrics().density);
    }
}
