package dev.istmo.runtime

import android.app.Activity

/**
 * Implemented by an activity-bound plugin backend (`android_backend_arg =
 * "activity"`) that outlives its activity: it keeps the activity weakly
 * and the generated registry hands it each new one through
 * [attachActivity], so the backend and its state are built once per
 * process instead of on every activity `onCreate`.
 *
 * A backend that does not implement it is rebuilt for every activity and
 * unregistered once the last one is destroyed. That stays the right
 * choice for backends that must register activity-result launchers
 * before their activity starts.
 */
interface IstmoActivityBound {
    /**
     * [activity] (already of the class the plugin declares) now hosts the
     * backend. Called on the main thread from the activity's `onCreate`,
     * or when the previous host is destroyed while another activity lives.
     */
    fun attachActivity(activity: Activity)
}
