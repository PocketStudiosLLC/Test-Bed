package org.androidcontroller.keeper;

import android.content.Context;
import android.content.SharedPreferences;
import android.view.View;
import android.widget.TextView;

/**
 * Makes a heading open and close the view under it, with ▾ (open) or ▸
 * (closed) before its text. Whether each is open is remembered by its key.
 */
final class Dropdown {
    private static final String PREFS = "sections";

    private Dropdown() {}

    static void attach(Context c, TextView heading, View body, String key) {
        attach(c, heading, heading, body, key);
    }

    /** {@code target} (the heading, or a row holding it) is what is tapped. */
    static void attach(Context c, View target, TextView heading, View body, String key) {
        SharedPreferences prefs = c.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        String title = heading.getText().toString();
        Runnable render = () -> {
            boolean open = body.getVisibility() == View.VISIBLE;
            heading.setText((open ? "▾  " : "▸  ") + title);
            heading.setContentDescription(title + (open ? ", open" : ", closed"));
        };
        body.setVisibility(prefs.getBoolean(key, true) ? View.VISIBLE : View.GONE);
        render.run();
        target.setClickable(true);
        target.setOnClickListener(v -> {
            boolean open = body.getVisibility() != View.VISIBLE;
            body.setVisibility(open ? View.VISIBLE : View.GONE);
            prefs.edit().putBoolean(key, open).apply();
            render.run();
        });
    }
}
