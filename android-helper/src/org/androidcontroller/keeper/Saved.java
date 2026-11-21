package org.androidcontroller.keeper;

import android.Manifest;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.media.MediaScannerConnection;
import android.net.Uri;
import android.os.Environment;
import android.provider.MediaStore;
import android.webkit.MimeTypeMap;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * Puts files where people look for them: Downloads for files from a PC,
 * Pictures/Screenshots for screenshots. Android 10+ goes through MediaStore
 * (no permission needed); older phones write the public folder when storage
 * access was granted, and the app's own folder otherwise.
 */
final class Saved {
    private Saved() {}

    /** Whether this phone needs the storage permission to write public folders. */
    static boolean needsPermission(Context c) {
        return !Keeper.isAtLeast(29)
                && c.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) != PackageManager.PERMISSION_GRANTED;
    }

    /** Copies {@code src} into Downloads as {@code name}; returns where it went, for people. */
    static String download(Context c, File src, String name) throws IOException {
        try (InputStream in = new FileInputStream(src)) {
            return save(c, in, name, mime(name), false);
        }
    }

    /** A PNG into Pictures/Screenshots. */
    static String screenshot(Context c, byte[] png, String name) throws IOException {
        return save(c, new java.io.ByteArrayInputStream(png), name, "image/png", true);
    }

    private static String save(Context c, InputStream in, String name, String mime, boolean picture) throws IOException {
        String folder = picture ? Environment.DIRECTORY_PICTURES + "/Screenshots" : Environment.DIRECTORY_DOWNLOADS;
        if (Keeper.isAtLeast(29)) {
            ContentResolver cr = c.getContentResolver();
            ContentValues v = new ContentValues();
            v.put(MediaStore.MediaColumns.DISPLAY_NAME, name);
            v.put(MediaStore.MediaColumns.MIME_TYPE, mime);
            v.put(MediaStore.MediaColumns.RELATIVE_PATH, folder);
            v.put(MediaStore.MediaColumns.IS_PENDING, 1);
            Uri collection = picture ? MediaStore.Images.Media.EXTERNAL_CONTENT_URI : MediaStore.Downloads.EXTERNAL_CONTENT_URI;
            Uri item = cr.insert(collection, v);
            if (item == null) throw new IOException("Android didn't make room for " + name);
            try (OutputStream out = cr.openOutputStream(item)) {
                if (out == null) throw new IOException("can't write " + name);
                copy(in, out);
            } catch (IOException | RuntimeException e) {
                cr.delete(item, null, null);
                throw e;
            }
            v.clear();
            v.put(MediaStore.MediaColumns.IS_PENDING, 0);
            cr.update(item, v, null, null);
            // Android renames it if the name was taken.
            String shown = name;
            try (Cursor q = cr.query(item, new String[] {MediaStore.MediaColumns.DISPLAY_NAME}, null, null, null)) {
                if (q != null && q.moveToFirst() && q.getString(0) != null) shown = q.getString(0);
            }
            return folder + "/" + shown;
        }
        File dir = needsPermission(c)
                ? c.getExternalFilesDir(picture ? Environment.DIRECTORY_PICTURES : Environment.DIRECTORY_DOWNLOADS)
                : Environment.getExternalStoragePublicDirectory(folder);
        if (dir == null) dir = new File(c.getFilesDir(), picture ? "Screenshots" : "Downloads");
        if (!dir.isDirectory() && !dir.mkdirs()) throw new IOException("can't make " + dir);
        File out = unique(dir, name);
        try (OutputStream o = new FileOutputStream(out)) {
            copy(in, o);
        }
        MediaScannerConnection.scanFile(c, new String[] {out.getAbsolutePath()}, new String[] {mime}, null);
        return out.getAbsolutePath();
    }

    /** {@code name}, or "name (2).ext" and so on if taken. */
    static File unique(File dir, String name) {
        File f = new File(dir, name);
        int dot = name.lastIndexOf('.');
        String stem = dot > 0 ? name.substring(0, dot) : name, ext = dot > 0 ? name.substring(dot) : "";
        for (int i = 2; f.exists(); i++) f = new File(dir, stem + " (" + i + ")" + ext);
        return f;
    }

    /** A name that is safe as one file name. */
    static String safeName(String name) {
        String s = name == null ? "" : name.replaceAll("[\\\\/:*?\"<>|\\x00-\\x1f]", "_").trim();
        return s.isEmpty() || s.equals(".") || s.equals("..") ? "file" : s;
    }

    static String mime(String name) {
        int dot = name.lastIndexOf('.');
        String ext = dot >= 0 ? name.substring(dot + 1).toLowerCase(java.util.Locale.ROOT) : "";
        String m = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext);
        return m == null ? "application/octet-stream" : m;
    }

    static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buf = new byte[64 * 1024];
        for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
    }

    /** Deletes {@code f} and, for a folder, everything in it. */
    static void delete(File f) {
        File[] kids = f.listFiles();
        if (kids != null) for (File k : kids) delete(k);
        //noinspection ResultOfMethodCallIgnored
        f.delete();
    }
}
