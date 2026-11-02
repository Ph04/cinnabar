package @APPLICATION_ID@;

import android.content.Context;
import android.os.Build;
import android.os.Process;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.PrintWriter;
import java.io.RandomAccessFile;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;

/** Bounded local records for failures deliberately caught across Java/JNI setup boundaries. */
final class RuntimeDiagnostics {
    static final int MAX_FILE_BYTES = 64 * 1024;
    static final int MAX_ENTRY_BYTES = 48 * 1024;
    static final int MAX_DISPLAY_BYTES = 16 * 1024;

    private RuntimeDiagnostics() { }

    /** Stores a caught Java exception with its complete bounded stack trace. */
    static void record(Context context, String stage, Throwable error) {
        if (error == null) return;
        recordText(context, stage, describe(error));
    }

    /** Stores already formatted Java or JNI details without network access or logcat dependency. */
    static void recordText(Context context, String stage, String details) {
        if (context == null || details == null || details.isEmpty()) return;
        File logs = new File(new File(context.getFilesDir(), "data"), "logs");
        String name = stage != null && stage.startsWith("JNI/")
                ? "jni-errors.log" : "bootstrap-errors.log";
        write(new File(logs, name), stage, details, System.currentTimeMillis(),
                Process.myPid(), Thread.currentThread().getName(),
                Build.VERSION.RELEASE + " API " + Build.VERSION.SDK_INT + " "
                        + Build.MANUFACTURER + "/" + Build.MODEL);
    }

    /** Converts a throwable to a size-limited stack trace suitable for the setup dialog. */
    static String describe(Throwable error) {
        if (error == null) return "Unknown setup failure";
        try {
            StringWriter text = new StringWriter();
            PrintWriter output = new PrintWriter(text);
            error.printStackTrace(output);
            output.flush();
            String value = text.toString();
            int limit = MAX_ENTRY_BYTES / 2;
            if (value.length() > limit) {
                value = value.substring(0, limit) + "\n[exception stack trace truncated]\n";
            }
            return value;
        } catch (Throwable ignored) {
            return error.getClass().getName() + ": stack trace unavailable";
        }
    }

    /** Atomic bounded append. Oldest records are discarded before the destination is replaced. */
    static synchronized void write(
            File destination, String stage, String details, long timestamp,
            int processId, String threadName, String runtime) {
        if (destination == null || details == null) return;
        File parent = destination.getParentFile();
        if (parent == null || (!parent.isDirectory() && !parent.mkdirs())) return;
        String header = "\n== " + timestamp + " stage=" + safe(stage)
                + " pid=" + processId + " thread=" + safe(threadName)
                + " runtime=" + safe(runtime) + " ==\n";
        byte[] entry = (header + details + (details.endsWith("\n") ? "" : "\n"))
                .getBytes(StandardCharsets.UTF_8);
        if (entry.length > MAX_ENTRY_BYTES) entry = tail(entry, MAX_ENTRY_BYTES);
        byte[] previous = readTail(destination, Math.max(0, MAX_FILE_BYTES - entry.length));
        byte[] combined = new byte[previous.length + entry.length];
        System.arraycopy(previous, 0, combined, 0, previous.length);
        System.arraycopy(entry, 0, combined, previous.length, entry.length);
        File temporary = new File(parent, destination.getName() + ".tmp");
        try (FileOutputStream output = new FileOutputStream(temporary, false)) {
            output.write(combined);
            output.getFD().sync();
        } catch (IOException error) {
            temporary.delete();
            return;
        }
        if (!temporary.renameTo(destination)) {
            if (destination.exists() && destination.delete()) temporary.renameTo(destination);
        }
        if (temporary.exists()) temporary.delete();
    }

    /** Returns a short bounded UTF-8 tail for the startup dialog; missing files are harmless. */
    static String readTail(File file) {
        return new String(readTail(file, MAX_DISPLAY_BYTES), StandardCharsets.UTF_8);
    }

    private static byte[] readTail(File file, int maximum) {
        if (file == null || maximum <= 0) return new byte[0];
        try (RandomAccessFile input = new RandomAccessFile(file, "r")) {
            int count = (int) Math.min(input.length(), maximum);
            input.seek(input.length() - count);
            byte[] bytes = new byte[count];
            input.readFully(bytes);
            return tail(bytes, maximum);
        } catch (IOException error) {
            return new byte[0];
        }
    }

    /** Drops a partial leading UTF-8 code point from a byte tail. */
    private static byte[] tail(byte[] bytes, int maximum) {
        int start = Math.max(0, bytes.length - maximum);
        while (start < bytes.length && (bytes[start] & 0xc0) == 0x80) start++;
        int count = bytes.length - start;
        byte[] result = new byte[count];
        System.arraycopy(bytes, start, result, 0, count);
        return result;
    }

    /** Prevents a metadata field from injecting additional record headers. */
    private static String safe(String value) {
        if (value == null) return "unknown";
        return value.replace('\n', ' ').replace('\r', ' ').replace('=', ':');
    }
}
