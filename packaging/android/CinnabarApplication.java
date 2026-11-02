package @APPLICATION_ID@;

import android.app.Application;
import android.os.Build;
import android.os.Process;
import java.io.File;

/** Installs early Java exception capture for both the setup and native game activities. */
public final class CinnabarApplication extends Application {
    @Override public void onCreate() {
        super.onCreate();
        final File crashLog = new File(
                new File(new File(getFilesDir(), "data"), "logs"), "java-crash.log");
        final int processId = Process.myPid();
        final String runtime = getPackageName()
                + " pid=" + processId
                + " Android=" + Build.VERSION.RELEASE
                + " (API " + Build.VERSION.SDK_INT + ") "
                + Build.MANUFACTURER + "/" + Build.MODEL;
        final Thread.UncaughtExceptionHandler previous =
                Thread.getDefaultUncaughtExceptionHandler();
        Thread.setDefaultUncaughtExceptionHandler(new Thread.UncaughtExceptionHandler() {
            @Override public void uncaughtException(Thread thread, Throwable error) {
                JavaCrashCapture.write(
                        crashLog, thread, error, System.currentTimeMillis(), processId, runtime);
                if (previous != null) {
                    previous.uncaughtException(thread, error);
                } else {
                    android.util.Log.e("@PRODUCT_NAME@", "Uncaught Java exception", error);
                    Process.killProcess(Process.myPid());
                    System.exit(10);
                }
            }
        });
    }
}
