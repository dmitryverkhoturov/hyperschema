package com.hyperschema.app

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.net.Uri
import android.os.Bundle
import android.view.Choreographer
import android.view.GestureDetector
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.VelocityTracker
import android.widget.FrameLayout
import android.widget.TextView
import android.widget.Toast
import kotlin.math.exp
import kotlin.math.hypot

/**
 * Thin shell: a SurfaceView the Rust engine renders into, gestures mapped
 * to pan/zoom, frames driven by Choreographer only while something moves.
 */
class MainActivity : Activity(), SurfaceHolder.Callback, Choreographer.FrameCallback {
    private var handle = 0L
    private var creating = false
    private var attached = false
    private lateinit var surfaceView: SurfaceView
    private lateinit var hud: TextView
    private var scheduled = false
    private var dirty = false
    private var pendingFile: String? = null

    // Gesture state: centroid and mean distance to it over all fingers.
    private var lastCx = 0f
    private var lastCy = 0f
    private var lastSpan = 0f
    private var lastCount = 0
    private var velocity: VelocityTracker? = null
    private var flingVx = 0f
    private var flingVy = 0f
    private var lastFrameNs = 0L

    private var frames = 0
    private var fpsT = 0L
    private lateinit var taps: GestureDetector

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        hud = TextView(this).apply {
            setTextColor(Color.argb(200, 40, 40, 40))
            setBackgroundColor(Color.argb(150, 255, 255, 255))
            textSize = 11f
            setPadding(16, 8, 16, 8)
            text = "Загрузка…"
        }
        val root = FrameLayout(this)
        root.addView(surfaceView)
        root.addView(hud, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.START).apply { topMargin = 90; leftMargin = 24 })
        setContentView(root)
        taps = GestureDetector(this, object : GestureDetector.SimpleOnGestureListener() {
            override fun onDoubleTap(e: MotionEvent): Boolean {
                if (handle != 0L) Native.zoom(handle, 2f, e.x, e.y)
                request()
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                if (handle != 0L) Native.fit(handle)
                request()
            }
        })
        surfaceView.setOnTouchListener { _, ev -> onTouch(ev); true }
        readIntent(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        readIntent(intent)
    }

    private fun readIntent(intent: Intent?) {
        val uri: Uri? = when (intent?.action) {
            Intent.ACTION_VIEW -> intent.data
            @Suppress("DEPRECATION")
            Intent.ACTION_SEND -> intent.getParcelableExtra(Intent.EXTRA_STREAM)
            else -> null
        }
        val text = intent?.getStringExtra(Intent.EXTRA_TEXT)
        pendingFile = when {
            uri != null -> runCatching { contentResolver.openInputStream(uri)?.use { it.readBytes().toString(Charsets.UTF_8) } }.getOrNull()
            text != null -> text
            else -> null
        }
        loadPending()
    }

    private fun loadPending() {
        val text = pendingFile ?: return
        if (handle == 0L) return
        pendingFile = null
        val err = Native.load(handle, text)
        if (err != null) Toast.makeText(this, "Не открылось: $err", Toast.LENGTH_LONG).show()
        request()
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {
        when {
            handle == 0L && !creating -> {
                creating = true
                val surface = holder.surface
                // Generating and indexing the test scene takes a moment: off the UI thread.
                Thread {
                    val hnd = Native.create(surface, w, h, 500_000)
                    runOnUiThread {
                        handle = hnd
                        creating = false
                        attached = hnd != 0L
                        loadPending()
                        request()
                    }
                }.start()
            }
            handle != 0L && !attached -> {
                Native.attach(handle, holder.surface, w, h)
                attached = true
            }
            handle != 0L -> Native.resize(handle, w, h)
        }
        request()
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        if (handle != 0L) Native.detach(handle)
        attached = false
    }

    private fun request() {
        dirty = true
        if (!scheduled) {
            scheduled = true
            Choreographer.getInstance().postFrameCallback(this)
        }
    }

    override fun doFrame(frameTimeNanos: Long) {
        scheduled = false
        if (handle == 0L || !attached) return
        val dt = if (lastFrameNs == 0L) 0.016f else ((frameTimeNanos - lastFrameNs) / 1e9f).coerceIn(0f, 0.05f)
        lastFrameNs = frameTimeNanos
        if (flingVx != 0f || flingVy != 0f) {
            Native.pan(handle, flingVx * dt, flingVy * dt)
            // Short glide (~0.25 s time constant), no long drift.
            val k = exp(-dt / 0.25f)
            flingVx *= k
            flingVy *= k
            if (hypot(flingVx, flingVy) < 30f) {
                flingVx = 0f
                flingVy = 0f
            }
            dirty = true
        }
        if (dirty) {
            dirty = false
            Native.render(handle)
            frames++
            if (flingVx != 0f || flingVy != 0f) request()
        }
        if (frameTimeNanos - fpsT > 500_000_000L) {
            val fps = frames * 1e9f / (frameTimeNanos - fpsT).coerceAtLeast(1)
            hud.text = "%.0f fps · %s".format(fps, Native.stats(handle))
            frames = 0
            fpsT = frameTimeNanos
        }
    }

    private fun onTouch(ev: MotionEvent) {
        taps.onTouchEvent(ev)
        if (handle == 0L) return
        val vt = velocity ?: VelocityTracker.obtain().also { velocity = it }
        vt.addMovement(ev)
        val action = ev.actionMasked
        val skip = if (action == MotionEvent.ACTION_POINTER_UP) ev.actionIndex else -1
        var sx = 0f
        var sy = 0f
        var n = 0
        for (i in 0 until ev.pointerCount) {
            if (i == skip) continue
            sx += ev.getX(i)
            sy += ev.getY(i)
            n++
        }
        if (n == 0) n = 1
        val cx = sx / n
        val cy = sy / n
        var span = 0f
        for (i in 0 until ev.pointerCount) {
            if (i == skip) continue
            span += hypot(ev.getX(i) - cx, ev.getY(i) - cy)
        }
        span /= n
        when (action) {
            MotionEvent.ACTION_DOWN -> {
                flingVx = 0f
                flingVy = 0f
            }
            MotionEvent.ACTION_MOVE -> if (n == lastCount) {
                Native.pan(handle, cx - lastCx, cy - lastCy)
                if (n >= 2 && lastSpan > 1f && span > 1f) Native.zoom(handle, span / lastSpan, cx, cy)
                request()
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (action == MotionEvent.ACTION_UP && lastCount == 1) {
                    vt.computeCurrentVelocity(1000)
                    val vx = vt.xVelocity
                    val vy = vt.yVelocity
                    if (hypot(vx, vy) > 400f) {
                        flingVx = vx
                        flingVy = vy
                        lastFrameNs = 0L
                        request()
                    }
                }
                vt.recycle()
                velocity = null
                n = 0
            }
        }
        lastCx = cx
        lastCy = cy
        lastSpan = span
        lastCount = n
    }
}
