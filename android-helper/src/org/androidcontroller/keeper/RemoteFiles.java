package org.androidcontroller.keeper;

import android.app.AlertDialog;
import android.content.Context;
import android.content.DialogInterface;
import android.graphics.Color;
import android.text.TextUtils;
import android.text.format.Formatter;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * Receive files: a browser of a remote PC's folders, as on Test Bed for PCs.
 * The PC lists one folder at a time; the files ticked are copied to the
 * phone's Downloads by the viewer.
 */
final class RemoteFiles {
    private static final int TEXT = Ui.FG;
    private static final int DIM = Ui.DIM;

    interface Ask {
        /** List folder {@code dir} (home if empty), or its subfolder {@code name} (may be null). */
        void list(long id, String dir, String name);

        /** Copy these files of folder {@code dir} to the phone. */
        void take(String dir, List<String> names);

        void closed();
    }

    private final Context c;
    private final Ask ask;
    private final AlertDialog dialog;
    private final TextView path, status, omitted;
    private final LinearLayout list;
    private final ScrollView scroller;
    private final Button up, places;
    private final CheckBox showHidden;
    private long nextId, waiting;
    private JSONObject listing;
    private final List<String[]> placeList = new ArrayList<>();
    private final Set<String> picked = new LinkedHashSet<>();

    RemoteFiles(Context c, String hostName, Ask ask) {
        this.c = c;
        this.ask = ask;

        LinearLayout body = new LinearLayout(c);
        body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(dp(16), dp(4), dp(16), 0);

        LinearLayout nav = new LinearLayout(c);
        nav.setGravity(Gravity.CENTER_VERTICAL);
        up = small("↑ Up");
        up.setEnabled(false);
        up.setOnClickListener(v -> {
            String parent = listing == null || listing.isNull("parent") ? null : listing.optString("parent");
            if (parent != null) request(parent, null);
        });
        nav.addView(up);
        places = small("Go to…");
        places.setEnabled(false);
        places.setOnClickListener(v -> choosePlace());
        nav.addView(places);
        body.addView(nav);

        path = new TextView(c);
        path.setTextColor(DIM);
        path.setTextSize(12);
        path.setTypeface(android.graphics.Typeface.MONOSPACE);
        path.setSingleLine(true);
        path.setEllipsize(TextUtils.TruncateAt.START);
        path.setPadding(0, dp(4), 0, dp(4));
        body.addView(path);

        status = new TextView(c);
        status.setTextColor(DIM);
        status.setGravity(Gravity.CENTER);
        status.setPadding(0, dp(24), 0, dp(24));
        status.setText("Loading…");
        body.addView(status);

        list = new LinearLayout(c);
        list.setOrientation(LinearLayout.VERTICAL);
        scroller = new ScrollView(c);
        scroller.addView(list);
        scroller.setVisibility(View.GONE);
        body.addView(scroller, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout footer = new LinearLayout(c);
        footer.setGravity(Gravity.CENTER_VERTICAL);
        showHidden = new CheckBox(c);
        showHidden.setText("Show hidden files");
        Ui.style(showHidden);
        showHidden.setOnCheckedChangeListener((b, on) -> fill());
        footer.addView(showHidden, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        omitted = new TextView(c);
        omitted.setTextColor(DIM);
        omitted.setTextSize(12);
        footer.addView(omitted);
        body.addView(footer);

        dialog = new AlertDialog.Builder(c, android.R.style.Theme_DeviceDefault_Dialog_Alert)
                .setTitle("Receive files from " + hostName)
                .setView(body)
                .setPositiveButton("Receive", null)
                .setNegativeButton("Cancel", null)
                .create();
        dialog.setOnDismissListener(d -> ask.closed());
        dialog.setOnShowListener(d -> {
            Button receive = dialog.getButton(DialogInterface.BUTTON_POSITIVE);
            Ui.style(receive, Ui.Kind.SUGGESTED);
            receive.setAllCaps(false);
            receive.setPadding(dp(16), 0, dp(16), 0);
            Button cancel = dialog.getButton(DialogInterface.BUTTON_NEGATIVE);
            Ui.style(cancel, Ui.Kind.NORMAL);
            cancel.setAllCaps(false);
            cancel.setPadding(dp(16), 0, dp(16), 0);
            if (cancel.getLayoutParams() instanceof android.view.ViewGroup.MarginLayoutParams) {
                ((android.view.ViewGroup.MarginLayoutParams) cancel.getLayoutParams()).setMarginEnd(dp(8));
            }
            receive.setOnClickListener(v -> {
                if (picked.isEmpty() || listing == null) return;
                ask.take(listing.optString("path"), new ArrayList<>(picked));
                dialog.dismiss();
            });
            updateReceive();
        });
        dialog.show();
        // Tall enough to browse; the list scrolls.
        if (dialog.getWindow() != null) {
            int h = Math.round(c.getResources().getDisplayMetrics().heightPixels * 0.9f);
            dialog.getWindow().setLayout(android.view.ViewGroup.LayoutParams.MATCH_PARENT, h);
        }
        request("", null);
    }

    void dismiss() {
        dialog.dismiss();
    }

    private void request(String dir, String name) {
        waiting = ++nextId;
        status.setText("Loading…");
        ask.list(waiting, dir, name);
    }

    /** The PC's answer: {@code {"listing": …}} or {@code {"error": text}}. */
    void listing(long id, JSONObject o) {
        if (id != waiting) return;
        if (o.has("error")) {
            String e = o.optString("error");
            if (listing != null) {
                Toast.makeText(c, e, Toast.LENGTH_LONG).show();
                status.setVisibility(View.GONE);
            } else {
                showStatus("Can't show this PC's files\n\n" + e);
            }
            return;
        }
        JSONObject l = o.optJSONObject("listing");
        if (l == null) return;
        if (placeList.isEmpty()) {
            JSONArray ps = l.optJSONArray("places");
            for (int i = 0; ps != null && i < ps.length(); i++) {
                JSONObject p = ps.optJSONObject(i);
                if (p != null) placeList.add(new String[] {p.optString("name"), p.optString("path")});
            }
            places.setEnabled(!placeList.isEmpty());
        }
        listing = l;
        picked.clear();
        path.setText(l.optString("path"));
        up.setEnabled(!l.isNull("parent") && l.has("parent"));
        int n = l.optInt("omitted");
        omitted.setText(n == 0 ? "" : n + " more not shown");
        fill();
    }

    private void choosePlace() {
        String[] names = new String[placeList.size()];
        for (int i = 0; i < names.length; i++) names[i] = placeList.get(i)[0];
        new AlertDialog.Builder(c, android.R.style.Theme_DeviceDefault_Dialog_Alert)
                .setTitle("Go to")
                .setItems(names, (d, which) -> request(placeList.get(which)[1], null))
                .show();
    }

    private void showStatus(String text) {
        status.setText(text);
        status.setVisibility(View.VISIBLE);
        scroller.setVisibility(View.GONE);
    }

    /** Rows for the current listing. */
    private void fill() {
        list.removeAllViews();
        if (listing == null) return;
        boolean hidden = showHidden.isChecked();
        JSONArray es = listing.optJSONArray("entries");
        Set<String> shownFiles = new java.util.HashSet<>();
        int shown = 0;
        for (int i = 0; es != null && i < es.length(); i++) {
            JSONObject e = es.optJSONObject(i);
            if (e == null || (!hidden && e.optBoolean("hidden"))) continue;
            if (!e.optBoolean("dir")) shownFiles.add(e.optString("name"));
            list.addView(row(e));
            shown++;
        }
        // Ticks on files no longer shown don't count.
        picked.retainAll(shownFiles);
        if (shown == 0) {
            showStatus("Nothing here");
        } else {
            status.setVisibility(View.GONE);
            scroller.setVisibility(View.VISIBLE);
            scroller.scrollTo(0, 0);
        }
        updateReceive();
    }

    private View row(JSONObject e) {
        String name = e.optString("name");
        boolean dir = e.optBoolean("dir");
        LinearLayout line = new LinearLayout(c);
        line.setGravity(Gravity.CENTER_VERTICAL);
        line.setPadding(dp(4), dp(10), dp(4), dp(10));
        line.setClickable(true);
        line.setBackgroundResource(android.R.drawable.list_selector_background);

        TextView icon = new TextView(c);
        icon.setText(dir ? "📁" : "📄");
        icon.setTextSize(16);
        icon.setPadding(0, 0, dp(10), 0);
        line.addView(icon);

        LinearLayout labels = new LinearLayout(c);
        labels.setOrientation(LinearLayout.VERTICAL);
        TextView t = new TextView(c);
        t.setText(name);
        t.setTextColor(e.optBoolean("hidden") ? DIM : TEXT);
        t.setTextSize(15);
        t.setSingleLine(true);
        t.setEllipsize(TextUtils.TruncateAt.MIDDLE);
        labels.addView(t);
        String when = "";
        if (!e.isNull("modified") && e.has("modified")) {
            when = java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM).format(new java.util.Date(e.optLong("modified") * 1000));
        }
        String detail = dir ? when : Formatter.formatShortFileSize(c, e.optLong("size")) + (when.isEmpty() ? "" : " · " + when);
        if (!detail.isEmpty()) {
            TextView d = new TextView(c);
            d.setText(detail);
            d.setTextColor(DIM);
            d.setTextSize(12);
            labels.addView(d);
        }
        line.addView(labels, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));

        if (dir) {
            TextView arrow = new TextView(c);
            arrow.setText("›");
            arrow.setTextSize(22);
            arrow.setTextColor(DIM);
            arrow.setPadding(dp(8), 0, dp(4), 0);
            line.addView(arrow);
            line.setOnClickListener(v -> {
                if (listing != null) request(listing.optString("path"), name);
            });
        } else {
            CheckBox check = new CheckBox(c);
            Ui.style(check);
            check.setChecked(picked.contains(name));
            check.setOnCheckedChangeListener((b, on) -> {
                if (on) picked.add(name);
                else picked.remove(name);
                updateReceive();
            });
            line.addView(check);
            line.setOnClickListener(v -> check.toggle());
        }
        return line;
    }

    private void updateReceive() {
        Button receive = dialog == null ? null : dialog.getButton(DialogInterface.BUTTON_POSITIVE);
        if (receive == null) return;
        int n = picked.size();
        receive.setEnabled(n > 0);
        receive.setText(n <= 1 ? "Receive" : "Receive " + n + " files");
    }

    private Button small(String label) {
        Button b = Ui.button(c, label, Ui.Kind.FLAT);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, dp(36));
        lp.setMarginEnd(dp(6));
        b.setLayoutParams(lp);
        return b;
    }

    private int dp(int v) {
        return Math.round(v * c.getResources().getDisplayMetrics().density);
    }
}
