package @APPLICATION_ID@;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.io.RandomAccessFile;
import java.io.Writer;
import java.nio.charset.StandardCharsets;

/** Persists one bounded Java exception before Android's default handler terminates the process. */
final class JavaCrashCapture {
    static final int MAX_TAIL_BYTES = 70 * 1024;
    private static final int MAX_STACK_CHARS = 16 * 1024;
    private static final String TRUNCATION_MARKER = "\n[stack trace truncated]\n";

    private JavaCrashCapture() { }

    /**
     * Writes through a same-directory temporary file so an interrupted capture cannot replace
     * the last complete report. This method deliberately avoids Android services and logging.
     */
    static synchronized void write(
            File destination, Thread thread, Throwable failure, long timestamp, int processId, String runtime) {
        if (destination == null || thread == null || failure == null) return;
        File parent = destination.getParentFile();
        if (parent == null || (!parent.isDirectory() && !parent.mkdirs())) return;
        File temporary = new File(parent, destination.getName() + ".tmp");
        try (FileOutputStream stream = new FileOutputStream(temporary, false)) {
            BoundedWriter bounded = new BoundedWriter(
                    new OutputStreamWriter(stream, StandardCharsets.UTF_8),
                    MAX_STACK_CHARS - TRUNCATION_MARKER.length());
            PrintWriter output = new PrintWriter(bounded);
            output.println("Java uncaught exception");
            output.println("timestamp_ms=" + timestamp);
            output.println("process_id=" + processId);
            output.println("thread_name=" + thread.getName());
            output.println("thread_id=" + thread.getId());
            output.println("runtime=" + runtime);
            failure.printStackTrace(output);
            bounded.finish();
            output.flush();
            if (output.checkError()) throw new IOException("cannot write Java crash report");
            stream.getFD().sync();
            output.close();
        } catch (Throwable error) {
            temporary.delete();
            return;
        }
        if (!temporary.renameTo(destination)) {
            if (destination.exists() && destination.delete()) temporary.renameTo(destination);
        }
        if (temporary.exists()) temporary.delete();
    }

    /** Returns this Java report only when its wall-clock capture belongs to the Android exit. */
    static String readForExit(File file, long exitTimestamp, int exitProcessId) {
        try (RandomAccessFile source = new RandomAccessFile(file, "r")) {
            if (!"Java uncaught exception".equals(source.readLine())) return "";
            String timestamp = source.readLine();
            String process = source.readLine();
            if (timestamp == null || !timestamp.startsWith("timestamp_ms=")
                    || process == null || !process.startsWith("process_id=")) return "";
            int capturedProcessId = Integer.parseInt(process.substring("process_id=".length()));
            if (capturedProcessId != exitProcessId) return "";
            long captured = Long.parseLong(timestamp.substring("timestamp_ms=".length()));
            long difference = captured >= exitTimestamp
                    ? captured - exitTimestamp : exitTimestamp - captured;
            if (difference > 120_000L) return "";
            return readTail(file);
        } catch (IOException | NumberFormatException error) {
            return "";
        }
    }

    /** Reads only the newest bounded bytes and starts on a UTF-8 character boundary. */
    static String readTail(File file) {
        try (RandomAccessFile source = new RandomAccessFile(file, "r")) {
            int count = (int) Math.min(source.length(), MAX_TAIL_BYTES);
            source.seek(source.length() - count);
            byte[] bytes = new byte[count];
            source.readFully(bytes);
            int start = 0;
            while (start < bytes.length && (bytes[start] & 0xc0) == 0x80) start++;
            return new String(bytes, start, bytes.length - start, StandardCharsets.UTF_8);
        } catch (IOException error) {
            return "";
        }
    }

    /** Stops stack formatting at a fixed character budget while reserving room for its marker. */
    private static final class BoundedWriter extends Writer {
        private final Writer destination;
        private final int limit;
        private int written;
        private boolean truncated;

        BoundedWriter(Writer destination, int limit) {
            this.destination = destination;
            this.limit = limit;
        }

        @Override public void write(char[] chars, int offset, int length) throws IOException {
            if (offset < 0 || length < 0 || offset > chars.length - length)
                throw new IndexOutOfBoundsException();
            int accepted = Math.min(length, Math.max(0, limit - written));
            if (accepted > 0) {
                destination.write(chars, offset, accepted);
                written += accepted;
            }
            if (accepted < length) truncated = true;
        }

        @Override public void flush() throws IOException {
            destination.flush();
        }

        @Override public void close() throws IOException {
            destination.close();
        }

        void finish() throws IOException {
            if (truncated) destination.write(TRUNCATION_MARKER);
        }
    }
}
