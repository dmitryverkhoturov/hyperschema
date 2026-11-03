package com.hyperschema.app

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.OpenableColumns
import android.text.Editable
import android.text.InputType
import android.text.TextWatcher
import android.util.TypedValue
import android.view.Choreographer
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.TextView
import kotlin.math.max
import kotlin.math.min

/**
 * Thin shell: the Rust core (shared editor + finger gestures) does the
 * work; this draws a few controls over the canvas. Boards live in the
 * core's library (filesDir/boards) and sync through the server.
 *
 * No dialogs: text is typed straight into the concept (a hidden field feeds
 * the keyboard, the engine draws text and caret); a selection or a tapped
 * connection gets a small bar next to it. Gestures are described in
 * `core/src/touch.rs`. Changes save themselves half a second later.
 */
class MainActivity : Activity(), SurfaceHolder.Callback, Choreographer.FrameCallback {
    private var handle = 0L
    private var creating = false
    private var attached = false
    private lateinit var surfaceView: SurfaceView
    private lateinit var root: FrameLayout
    private lateinit var title: TextView
    private lateinit var undoBtn: TextView
    private lateinit var redoBtn: TextView
    private lateinit var plusBtn: TextView
    private lateinit var bar: LinearLayout
    private lateinit var field: Field
    private var scheduled = false
    private var dirty = false
    /** The open board (core `library.rs`) and its name. */
    private var boardId: String? = null
    private var boardName = ""
    /** The server is out of reach. */
    private var offline = false
    /** What waits for the core to start (a file handed in before it). */
    private val waiting = mutableListOf<() -> Unit>()
    private val poller = Runnable { pollSync() }
    /** Walks and their layout as last kept in preferences. */
    private var walksSaved = ""
    private var layoutSaved = ""
    private val ui = Handler(Looper.getMainLooper())
    private val saver = Runnable { save() }
    private var topInset = 0
    private var bottomInset = 0
    private var imeBottom = 0
    private var editing = false
    private var syncingField = false
    private var barKind = ""
    /** Screen box of the selection (or of the concept being typed into). */
    private var selBox: List<Float>? = null
    /** Selection style from the core: fill index (-1 other), shape, holds others, count. */
    private var style: List<Int> = listOf(-1, 0, 0, 1)
    /** The selection bar shows the colour dots instead of its commands. */
    private var pickingColor = false
    private lateinit var searchBox: LinearLayout
    private lateinit var searchField: EditText
    private lateinit var searchCount: TextView

    private val ink = Color.rgb(30, 30, 30)
    private val canvasBg = Color.rgb(247, 246, 243)

    private fun dp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, resources.displayMetrics)

    /** The keyboard's field: invisible, it only carries text to the core. */
    inner class Field(c: Context) : EditText(c) {
        override fun onKeyPreIme(keyCode: Int, event: KeyEvent): Boolean {
            if (keyCode == KeyEvent.KEYCODE_BACK && event.action == KeyEvent.ACTION_UP && editing) {
                call { Native.commitText(handle) }
                return true
            }
            return super.onKeyPreIme(keyCode, event)
        }
    }

    private fun round(text: String, size: Float, fill: Int, fg: Int, onClick: (View) -> Unit) = TextView(this).apply {
        this.text = text
        textSize = size
        setTextColor(fg)
        gravity = Gravity.CENTER
        isSoundEffectsEnabled = false
        background = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(fill)
            setStroke(dp(1f).toInt(), Color.argb(28, 0, 0, 0))
        }
        elevation = dp(2f)
        setOnClickListener { onClick(it) }
    }

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        // Edge to edge, bars see-through over the canvas colour, dark icons.
        window.setDecorFitsSystemWindows(false)
        window.statusBarColor = Color.TRANSPARENT
        window.navigationBarColor = Color.TRANSPARENT
        window.isNavigationBarContrastEnforced = false
        window.isStatusBarContrastEnforced = false
        window.setBackgroundDrawable(ColorDrawable(canvasBg))
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)

        root = FrameLayout(this)
        surfaceView = SurfaceView(this)
        surfaceView.holder.addCallback(this)
        // Hidden until the first frame: no black flash while the GPU starts.
        surfaceView.alpha = 0f
        root.addView(surfaceView)

        val glass = Color.argb(235, 255, 255, 255)
        val b = dp(44f).toInt()
        val menu = round("☰", 18f, glass, ink) { showMenu(it) }
        title = TextView(this).apply {
            textSize = 14f
            setTextColor(Color.rgb(90, 90, 90))
            setPadding(dp(10f).toInt(), 0, 0, 0)
            maxLines = 1
        }
        val left = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(menu, LinearLayout.LayoutParams(b, b))
            addView(title)
        }
        val searchBtn = round("⌕", 22f, glass, ink) { openSearch() }
        undoBtn = round("↶", 20f, glass, ink) { call { Native.undo(handle, false) } }
        redoBtn = round("↷", 20f, glass, ink) { call { Native.undo(handle, true) } }
        val right = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            addView(searchBtn, LinearLayout.LayoutParams(b, b))
            addView(View(this@MainActivity), LinearLayout.LayoutParams(dp(8f).toInt(), 1))
            addView(undoBtn, LinearLayout.LayoutParams(b, b))
            addView(View(this@MainActivity), LinearLayout.LayoutParams(dp(8f).toInt(), 1))
            addView(redoBtn, LinearLayout.LayoutParams(b, b))
        }
        root.addView(left, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.START))
        root.addView(right, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.END))
        plusBtn = round("+", 28f, ink, Color.WHITE) { call { Native.createCenter(handle) } }
        plusBtn.elevation = dp(4f)
        root.addView(plusBtn, FrameLayout.LayoutParams(dp(56f).toInt(), dp(56f).toInt(), Gravity.BOTTOM or Gravity.END))

        bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = GradientDrawable().apply {
                cornerRadius = dp(22f)
                setColor(Color.WHITE)
                setStroke(dp(1f).toInt(), Color.argb(24, 0, 0, 0))
            }
            elevation = dp(6f)
            visibility = View.GONE
            setPadding(dp(6f).toInt(), 0, dp(6f).toInt(), 0)
        }
        root.addView(bar, FrameLayout.LayoutParams(-2, dp(44f).toInt()))

        // Search: a field with the match count, previous, next, close.
        searchField = EditText(this).apply {
            hint = "Поиск по схеме"
            textSize = 15f
            background = null
            isSingleLine = true
            imeOptions = EditorInfo.IME_ACTION_SEARCH
            setPadding(dp(14f).toInt(), 0, dp(4f).toInt(), 0)
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun afterTextChanged(e: Editable?) = search(0)
            })
            setOnEditorActionListener { _, _, _ -> search(1); true }
        }
        searchCount = TextView(this).apply {
            textSize = 13f
            setTextColor(Color.rgb(120, 120, 120))
            gravity = Gravity.CENTER_VERTICAL
        }
        searchBox = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = GradientDrawable().apply {
                cornerRadius = dp(22f)
                setColor(Color.WHITE)
                setStroke(dp(1f).toInt(), Color.argb(24, 0, 0, 0))
            }
            elevation = dp(6f)
            visibility = View.GONE
            addView(searchField, LinearLayout.LayoutParams(0, -1, 1f))
            addView(searchCount, LinearLayout.LayoutParams(-2, -1))
            addView(plainButton("‹") { search(-1) }, LinearLayout.LayoutParams(-2, -1))
            addView(plainButton("›") { search(1) }, LinearLayout.LayoutParams(-2, -1))
            addView(plainButton("✕") { closeSearch() }, LinearLayout.LayoutParams(-2, -1))
        }
        root.addView(searchBox, FrameLayout.LayoutParams(-1, dp(44f).toInt(), Gravity.TOP))

        field = Field(this).apply {
            alpha = 0f
            isCursorVisible = false
            background = null
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun afterTextChanged(s: Editable?) {
                    if (syncingField || handle == 0L || !editing) return
                    Native.setText(handle, s.toString())
                    sync()
                }
            })
        }
        root.addView(field, FrameLayout.LayoutParams(dp(2f).toInt(), dp(2f).toInt(), Gravity.TOP or Gravity.START))

        root.setOnApplyWindowInsetsListener { _, insets ->
            val sys = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            topInset = sys.top
            bottomInset = sys.bottom
            if (handle != 0L) Native.setTop(handle, sys.top + dp(60f))
            imeBottom = insets.getInsets(WindowInsets.Type.ime()).bottom
            val m = dp(10f).toInt()
            (left.layoutParams as FrameLayout.LayoutParams).setMargins(m + sys.left, m + sys.top, 0, 0)
            (right.layoutParams as FrameLayout.LayoutParams).setMargins(0, m + sys.top, m + sys.right, 0)
            (plusBtn.layoutParams as FrameLayout.LayoutParams).setMargins(0, 0, dp(18f).toInt() + sys.right, dp(18f).toInt() + sys.bottom)
            (searchBox.layoutParams as FrameLayout.LayoutParams).setMargins(m + sys.left, m + sys.top + dp(52f).toInt(), m + sys.right, 0)
            left.requestLayout(); right.requestLayout(); plusBtn.requestLayout(); searchBox.requestLayout()
            keepTypingVisible()
            insets
        }
        setContentView(root)
        window.insetsController?.setSystemBarsAppearance(
            WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS,
            WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
        )

        surfaceView.setOnTouchListener { _, ev -> onTouch(ev); true }
        surfaceView.setOnGenericMotionListener { _, ev ->
            if (ev.actionMasked == MotionEvent.ACTION_SCROLL && handle != 0L) {
                Native.zoom(handle, Math.pow(2.0, ev.getAxisValue(MotionEvent.AXIS_VSCROLL) * 0.35).toFloat(), ev.x, ev.y)
                request()
                true
            } else false
        }

        // Debug: --es dbgfile NAME opens filesDir/NAME (emulator tests).
        val dbg = intent?.getStringExtra("dbgfile")
        if (dbg != null) open(Uri.fromFile(java.io.File(filesDir, dbg)))
        else readIntent(intent)
        updateButtons()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val g = intent.getStringExtra("gest")
        if (g != null) {
            debugGesture(g, intent.getFloatExtra("x", 540f), intent.getFloatExtra("y", 1200f), intent.getFloatExtra("dx", 0f), intent.getFloatExtra("dy", 300f))
            return
        }
        intent.getStringExtra("act")?.let { a ->
            // Debug: nc:N (selection bar choice), copy, paste, find:TEXT, next.
            when {
                a.startsWith("nc:") -> call { Native.nodeChoice(handle, a.substring(3).toInt()) }
                a == "copy" -> copySelection()
                a == "paste" -> paste()
                a.startsWith("find:") -> { openSearch(); searchField.setText(a.substring(5)) }
                a == "next" -> search(1)
                a == "colors" -> { pickingColor = true; sync() }
            }
            return
        }
        intent.getStringExtra("type")?.let { t ->
            // Debug: types into the hidden field as the keyboard would.
            field.text.append(t)
            return
        }
        val dbg = intent.getStringExtra("dbgfile")
        if (dbg != null) open(Uri.fromFile(java.io.File(filesDir, dbg))) else readIntent(intent)
    }

    @Deprecated("Back leaves the walk first")
    override fun onBackPressed() {
        if (handle != 0L && Native.pathExit(handle)) {
            sync()
            return
        }
        @Suppress("DEPRECATION")
        super.onBackPressed()
    }

    override fun onPause() {
        super.onPause()
        ui.removeCallbacks(poller)
        if (ui.hasCallbacks(saver)) {
            ui.removeCallbacks(saver)
            save()
        }
    }

    override fun onResume() {
        super.onResume()
        ui.removeCallbacks(poller)
        ui.post(poller)
    }

    /** Takes in other devices' changes about once a second while open. */
    private fun pollSync() {
        if (handle != 0L && boardId != null) {
            val r = Native.syncPoll(handle)
            val off = r and 2 != 0
            if (off != offline) {
                offline = off
                showTitle()
            }
            if (r and 1 != 0) sync()
        }
        ui.postDelayed(poller, 1000)
    }

    private fun showTitle() {
        title.text = if (offline) "$boardName · офлайн" else boardName
    }

    private fun readText(u: Uri): String? =
        runCatching { contentResolver.openInputStream(u)?.use { it.readBytes().toString(Charsets.UTF_8) } }.getOrNull()

    /** Opens what another app handed us; false if nothing came. */
    private fun readIntent(intent: Intent?): Boolean {
        if (intent?.action == Intent.ACTION_SEND) {
            intent.getStringExtra(Intent.EXTRA_TEXT)?.let { t ->
                whenReady { takeText(t, "Новая схема") }
                return true
            }
        }
        val u: Uri? = when (intent?.action) {
            Intent.ACTION_VIEW -> intent.data
            @Suppress("DEPRECATION")
            Intent.ACTION_SEND -> intent.getParcelableExtra(Intent.EXTRA_STREAM)
            else -> null
        }
        if (u != null) {
            open(u)
            return true
        }
        return false
    }

    /** A file from outside: a connection string, or a board to import. */
    private fun open(u: Uri) {
        val text = readText(u)
        if (text == null) return
        val name = displayName(u)
        whenReady { takeText(text, name) }
    }

    private fun takeText(text: String, name: String) {
        if (text.contains("\"password\"") && text.contains("\"url\"")) {
            Native.setSync(handle, text)
            return
        }
        val id = Native.importBoard(handle, name, text)
        if (!id.startsWith("!")) opened(id, name)
    }

    private fun whenReady(f: () -> Unit) {
        if (handle != 0L) f() else waiting.add(f)
    }

    private fun displayName(u: Uri): String {
        val name = runCatching {
            contentResolver.query(u, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c -> if (c.moveToFirst()) c.getString(0) else null }
        }.getOrNull() ?: u.lastPathSegment ?: ""
        return name.removeSuffix(".md").removeSuffix(".excalidraw").removeSuffix(".hyperschema").removeSuffix(".json").removeSuffix(".txt")
    }

    /** The core is up: open the library and the board open last time. */
    private fun startLibrary() {
        Native.setLibrary(handle, java.io.File(filesDir, "boards").path)
        val last = getPreferences(MODE_PRIVATE).getString("board", null)
        val name = last?.let { Native.openBoard(handle, it) }
        if (last != null && name != null) opened(last, name)
        for (f in waiting) f()
        waiting.clear()
        if (boardId == null) newBoard()
    }

    private fun openBoard(id: String) {
        save()
        val name = Native.openBoard(handle, id)
        if (name != null) opened(id, name)
    }

    /** A board just opened in the core: title, walks, controls. */
    private fun opened(id: String, name: String) {
        boardId = id
        boardName = name
        offline = false
        showTitle()
        val prefs = getPreferences(MODE_PRIVATE)
        prefs.edit().putString("board", id).apply()
        prefs.getString("layout", null)?.let { Native.setLayout(handle, it) }
        layoutSaved = Native.layout(handle)
        prefs.getString("walks:$id", null)?.let { Native.setWalks(handle, it) }
        walksSaved = Native.walks(handle)
        sync()
    }

    private fun save() {
        if (handle == 0L || boardId == null) return
        if (!Native.saveBoard(handle)) title.text = "$boardName · не сохранено"
    }

    /** This phone's boards and the server's. */
    private fun pickBoard() {
        if (handle == 0L) return
        val rows = Native.boards(handle).split('\n').filter { it.isNotBlank() }.map { it.split('\t') }
        val names = rows.map { if (it.getOrNull(2) == "1") "• ${it[1]}" else it[1] } + "Новая схема"
        android.app.AlertDialog.Builder(this)
            .setTitle("Схемы")
            .setItems(names.toTypedArray()) { _, k ->
                if (k < rows.size) openBoard(rows[k][0]) else newBoard()
            }
            .show()
    }

    private fun newBoard() {
        val names = Native.boards(handle).split('\n').map { it.split('\t').getOrElse(1) { "" } }.toSet()
        var n = "Новая схема"
        var k = 2
        while (n in names) n = "Новая схема ${k++}"
        takeText("", n)
    }

    /** Asks for the connection string (the contents of sync.json). */
    private fun connectDialog() {
        val input = EditText(this).apply {
            hint = "Строка подключения"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
            clipboard().primaryClip?.getItemAt(0)?.text?.toString()?.takeIf { it.contains("\"password\"") }?.let { setText(it) }
        }
        android.app.AlertDialog.Builder(this)
            .setTitle("Сервер синхронизации")
            .setView(input)
            .setPositiveButton("Сохранить") { _, _ -> takeText(input.text.toString(), "") }
            .setNegativeButton("Отмена", null)
            .show()
    }

    private fun nestedLinks() = getPreferences(MODE_PRIVATE).getBoolean("nested", false)

    private fun showMenu(anchor: View) {
        val m = PopupMenu(this, anchor)
        m.menu.add(0, 1, 0, "Схемы…")
        m.menu.add(0, 2, 1, "Новая схема")
        m.menu.add(0, 6, 2, "Открыть файл…")
        m.menu.add(0, 3, 3, "Вписать всё")
        m.menu.add(0, 4, 4, "Связи с вложенными").isCheckable = true
        if (clipboard().hasPrimaryClip() && Native.editable(handle)) m.menu.add(0, 5, 5, "Вставить")
        m.menu.add(0, 7, 6, "Сервер синхронизации…")
        m.menu.findItem(4).isChecked = nestedLinks()
        m.setOnMenuItemClickListener {
            when (it.itemId) {
                1 -> pickBoard()
                2 -> newBoard()
                6 -> startActivityForResult(Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*"), REQ_OPEN)
                3 -> call { Native.fit(handle) }
                5 -> paste()
                7 -> connectDialog()
                4 -> {
                    val on = !nestedLinks()
                    getPreferences(MODE_PRIVATE).edit().putBoolean("nested", on).apply()
                    call { Native.setNested(handle, on) }
                }
            }
            true
        }
        m.show()
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val u = data?.data ?: return
        if (resultCode != RESULT_OK) return
        if (requestCode == REQ_OPEN) open(u)
    }

    /** Runs a core call, then brings the controls in line with the core. */
    private fun call(f: () -> Unit) {
        if (handle == 0L) return
        f()
        sync()
    }

    /** Reads what the core wants shown: text field, bars, save. */
    private fun sync() {
        if (handle == 0L) return
        var text: String? = null
        var menu: List<String>? = null
        selBox = null
        for (line in Native.poll(handle).split('\n')) {
            val p = line.split('\t')
            when (p[0]) {
                "dirty" -> {
                    ui.removeCallbacks(saver)
                    ui.postDelayed(saver, 500)
                }
                "held" -> surfaceView.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                "text" -> text = line.substringAfter('\t').replace('\u0001', '\n')
                "sel" -> selBox = p.drop(1).map { it.toFloat() }
                "style" -> style = p.drop(1).map { it.toInt() }
                "menu" -> menu = p.drop(1)
            }
        }
        val imm = getSystemService(InputMethodManager::class.java)
        if (text != null && !editing) {
            editing = true
            syncingField = true
            field.setText(text)
            field.setSelection(field.text.length)
            syncingField = false
            field.requestFocus()
            imm.showSoftInput(field, 0)
        } else if (text != null && editing && text != field.text.toString()) {
            // The core changed the text (↶ while typing): the field follows.
            syncingField = true
            field.setText(text)
            field.setSelection(field.text.length)
            syncingField = false
        } else if (text == null && editing) {
            editing = false
            imm.hideSoftInputFromWindow(field.windowToken, 0)
            field.clearFocus()
        }
        val sel = selBox
        if (sel == null || editing) pickingColor = false
        when {
            menu != null -> showEdgeBar(menu)
            sel != null && !editing && pickingColor -> showColorBar(sel)
            sel != null && !editing -> showSelBar(sel)
            else -> hideBar()
        }
        if (editing) keepTypingVisible()
        plusBtn.visibility = if (editing || !Native.editable(handle)) View.GONE else View.VISIBLE
        updateButtons()
        keepWalks()
        request()
    }

    /** Remembers the board's walks and their layout when they changed. */
    private fun keepWalks() {
        val u = boardId ?: return
        val w = Native.walks(handle)
        val l = Native.layout(handle)
        if (w == walksSaved && l == layoutSaved) return
        walksSaved = w
        layoutSaved = l
        getPreferences(MODE_PRIVATE).edit().putString("walks:$u", w).putString("layout", l).apply()
    }

    /** The concept being typed into stays above the keyboard. */
    private fun keepTypingVisible() {
        val s = selBox ?: return
        if (!editing || handle == 0L) return
        val limit = root.height - max(imeBottom, bottomInset) - dp(24f)
        if (s[3] > limit) {
            Native.pan(handle, 0f, limit - s[3])
            request()
        }
    }

    private fun updateButtons() {
        val on = handle != 0L
        undoBtn.alpha = if (on && Native.canUndo(handle, false)) 1f else 0.35f
        redoBtn.alpha = if (on && Native.canUndo(handle, true)) 1f else 0.35f
    }

    private fun barButton(text: String, on: Boolean, f: () -> Unit) = TextView(this).apply {
        this.text = text
        textSize = 15f
        setTextColor(if (on) Color.rgb(0x2f, 0x5f, 0xe0) else ink)
        gravity = Gravity.CENTER
        isSoundEffectsEnabled = false
        setPadding(dp(12f).toInt(), 0, dp(12f).toInt(), 0)
        setOnClickListener { call(f) }
    }

    private fun placeBar(cx: Float, top: Float, bottom: Float) {
        bar.visibility = View.VISIBLE
        bar.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED)
        val w = bar.measuredWidth.toFloat()
        val h = dp(44f)
        // Clear of the connection dots around the selection.
        val gap = dp(46f)
        val above = top - gap - h
        val y = if (above > topInset + dp(64f)) above else min(bottom + gap, root.height - h - max(bottomInset, imeBottom) - gap)
        bar.x = (cx - w / 2).coerceIn(dp(8f), max(dp(8f), root.width - w - dp(8f)))
        bar.y = y
    }

    private fun showSelBar(s: List<Float>) {
        if (barKind != "sel") {
            bar.removeAllViews()
            bar.addView(barButton("Текст", false) { Native.editSelected(handle) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Путь", false) { Native.nodeChoice(handle, NODE_PATH) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Цвет", false) { pickingColor = true }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(plainButton("Форма") { shapeMenu(it) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(plainButton("Ещё") { moreMenu(it) }, LinearLayout.LayoutParams(-2, -1))
            barKind = "sel"
        }
        placeBar((s[0] + s[2]) / 2, s[1], s[3])
    }

    /** The fill colours as dots; a tap paints the selection. */
    private fun showColorBar(s: List<Float>) {
        val kind = "colors${style[0]}"
        if (barKind != kind) {
            bar.removeAllViews()
            for (k in PALETTE.indices) {
                val on = style[0] == k
                val dot = TextView(this).apply {
                    gravity = Gravity.CENTER
                    text = if (k == 0) "∅" else ""
                    textSize = 14f
                    setTextColor(Color.rgb(120, 120, 120))
                    background = GradientDrawable().apply {
                        shape = GradientDrawable.OVAL
                        setColor(PALETTE[k])
                        setStroke(dp(if (on) 2.5f else 1f).toInt(), if (on) Color.rgb(0x2f, 0x5f, 0xe0) else Color.argb(50, 0, 0, 0))
                    }
                    isSoundEffectsEnabled = false
                    setOnClickListener {
                        pickingColor = false
                        call { Native.nodeChoice(handle, NODE_COLOR + k) }
                    }
                }
                bar.addView(dot, LinearLayout.LayoutParams(dp(30f).toInt(), dp(30f).toInt()).apply { setMargins(dp(4f).toInt(), 0, dp(4f).toInt(), 0) })
            }
            barKind = kind
        }
        placeBar((s[0] + s[2]) / 2, s[1], s[3])
    }

    private fun shapeMenu(anchor: View) {
        val m = PopupMenu(this, anchor)
        val names = listOf("Прямоугольник", "Овал", "Ромб", "Текст без рамки")
        for (k in names.indices) {
            if (k == 3 && style[2] == 1) continue
            m.menu.add(0, NODE_SHAPE + k, k, names[k]).isCheckable = true
            m.menu.findItem(NODE_SHAPE + k).isChecked = style[1] == k
        }
        m.setOnMenuItemClickListener { call { Native.nodeChoice(handle, it.itemId) }; true }
        m.show()
    }

    private fun moreMenu(anchor: View) {
        val m = PopupMenu(this, anchor)
        m.menu.add(0, NODE_DUPLICATE, 0, "Дублировать")
        m.menu.add(0, 1, 1, "Копировать")
        m.menu.add(0, NODE_WRAP, 2, "Обернуть в блок")
        m.menu.add(0, NODE_DELETE, 3, "Удалить")
        if (style[2] == 1) m.menu.add(0, NODE_DELETE_DEEP, 4, "Удалить с содержимым")
        m.setOnMenuItemClickListener {
            if (it.itemId == 1) copySelection() else call { Native.nodeChoice(handle, it.itemId) }
            true
        }
        m.show()
    }

    private fun clipboard() = getSystemService(ClipboardManager::class.java)

    /** Our last copy (text, board): clipboard managers and the emulator's
     *  host sync may keep only the first, plain-text item. */
    private var lastCopy: Pair<String, String>? = null

    /** Plain text for other apps, the concepts themselves as a second item. */
    private fun copySelection() {
        if (handle == 0L) return
        val board = Native.copySelection(handle) ?: return
        val text = Native.selectionText(handle) ?: ""
        val clip = ClipData.newPlainText(CLIP_LABEL, text)
        clip.addItem(ClipData.Item(board))
        lastCopy = text to board
        clipboard().setPrimaryClip(clip)
    }

    private fun paste() {
        val clip = clipboard().primaryClip ?: return
        val ours = clip.description.label?.toString() == CLIP_LABEL && clip.itemCount > 1
        val text = clip.getItemAt(0).coerceToText(this)?.toString() ?: ""
        val board = when {
            ours -> clip.getItemAt(1).text?.toString() ?: ""
            lastCopy?.first == text -> lastCopy!!.second
            else -> ""
        }
        call { Native.paste(handle, board, text) }
    }

    private fun openSearch() {
        if (handle == 0L) return
        searchBox.visibility = View.VISIBLE
        searchField.requestFocus()
        getSystemService(InputMethodManager::class.java).showSoftInput(searchField, 0)
        search(0)
    }

    private fun closeSearch() {
        searchBox.visibility = View.GONE
        getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(searchField.windowToken, 0)
        searchField.clearFocus()
        call { Native.searchClose(handle) }
    }

    /** step: 0 the query changed, 1 next, -1 previous. */
    private fun search(step: Int) {
        if (handle == 0L || searchBox.visibility != View.VISIBLE) return
        val q = searchField.text.toString()
        var r = listOf("0", "0")
        call { r = Native.search(handle, q, step).split('\t') }
        val n = r[1].toInt()
        searchCount.text = when {
            q.isBlank() -> ""
            n == 0 -> "нет"
            else -> "${r[0].toInt() + 1} из $n"
        }
    }

    private fun plainButton(text: String, f: (View) -> Unit) = TextView(this).apply {
        this.text = text
        textSize = 15f
        setTextColor(ink)
        gravity = Gravity.CENTER
        isSoundEffectsEnabled = false
        setPadding(dp(12f).toInt(), 0, dp(12f).toInt(), 0)
        setOnClickListener { f(it) }
    }

    private fun showEdgeBar(m: List<String>) {
        val from = m[2] == "1"
        val to = m[3] == "1"
        val kind = "edge$from$to"
        if (barKind != kind) {
            bar.removeAllViews()
            bar.addView(barButton("—", !from && !to) { Native.edgeChoice(handle, 1) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("→", !from && to) { Native.edgeChoice(handle, 2) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("←", from && !to) { Native.edgeChoice(handle, 3) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("↔", from && to) { Native.edgeChoice(handle, 4) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Подпись", false) { Native.edgeChoice(handle, 5) }, LinearLayout.LayoutParams(-2, -1))
            bar.addView(barButton("Удалить", false) { Native.edgeChoice(handle, 6) }, LinearLayout.LayoutParams(-2, -1))
            barKind = kind
        }
        val x = m[0].toFloat()
        val y = m[1].toFloat()
        placeBar(x, y - dp(10f), y + dp(10f))
    }

    private fun hideBar() {
        bar.visibility = View.GONE
        barKind = ""
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {
        when {
            handle == 0L && !creating -> {
                creating = true
                val surface = holder.surface
                val density = resources.displayMetrics.density
                Thread {
                    val hnd = Native.create(surface, w, h, density)
                    runOnUiThread {
                        handle = hnd
                        creating = false
                        attached = hnd != 0L
                        if (hnd != 0L && nestedLinks()) Native.setNested(hnd, true)
                        if (hnd != 0L) Native.setTop(hnd, topInset + dp(60f))
                        if (hnd != 0L) startLibrary()
                        sync()
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
        val busy = Native.tick(handle, SystemClock.uptimeMillis())
        if (busy) sync()
        if (dirty || busy) {
            dirty = false
            if (Native.render(handle) && surfaceView.alpha < 1f) surfaceView.alpha = 1f
        }
        if (busy) request()
    }

    private fun onTouch(ev: MotionEvent) {
        if (handle == 0L) return
        val t = ev.eventTime
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val i = ev.actionIndex
                Native.touch(handle, 0, ev.getPointerId(i).toLong(), ev.getX(i), ev.getY(i), t)
            }
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until ev.pointerCount) Native.touch(handle, 1, ev.getPointerId(i).toLong(), ev.getX(i), ev.getY(i), t)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val i = ev.actionIndex
                Native.touch(handle, 2, ev.getPointerId(i).toLong(), ev.getX(i), ev.getY(i), t)
            }
            MotionEvent.ACTION_CANCEL -> Native.touch(handle, 3, 0, 0f, 0f, t)
        }
        sync()
    }

    /** Debug (adb): a synthetic gesture with exact timing — tap, dbl, qs, drag, long, ldrag, pinch. */
    private fun debugGesture(name: String, x: Float, y: Float, dx: Float, dy: Float) {
        val t0 = SystemClock.uptimeMillis() + 50
        fun ev(dt: Long, a: Int, px: Float, py: Float, id: Long = 0) {
            ui.postAtTime({ if (handle != 0L) { Native.touch(handle, a, id, px, py, t0 + dt); sync() } }, t0 + dt)
        }
        fun tap(at: Long) { ev(at, 0, x, y); ev(at + 60, 2, x, y) }
        fun line(at: Long) { for (i in 1..20) ev(at + i * 16L, 1, x + dx * i / 20, y + dy * i / 20); ev(at + 21 * 16L + 60, 2, x + dx, y + dy) }
        when (name) {
            "tap" -> tap(0)
            "dbl" -> { tap(0); tap(180) }
            "qs" -> { tap(0); ev(180, 0, x, y); line(180) }
            "drag" -> { ev(0, 0, x, y); line(0) }
            "long" -> { ev(0, 0, x, y); ev(700, 2, x, y) }
            "ldrag" -> { ev(0, 0, x, y); line(600) }
            "pinch" -> {
                ev(0, 0, x - 150, y, 0); ev(10, 0, x + 150, y, 1)
                for (i in 1..30) { val r = 150f * (1 + i / 30f); ev(10 + i * 16L, 1, x - r, y, 0); ev(10 + i * 16L, 1, x + r, y, 1) }
                ev(600, 2, x - 300, y, 0); ev(640, 2, x + 300, y, 1)
            }
        }
    }

    companion object {
        const val REQ_OPEN = 1
        const val REQ_NEW = 2
        const val CLIP_LABEL = "Hyperschema.Board"
        // Choices of core/src/editor.rs (NODE_*), fills of its PALETTE.
        const val NODE_COLOR = 10
        const val NODE_SHAPE = 20
        const val NODE_DUPLICATE = 31
        const val NODE_WRAP = 32
        const val NODE_DELETE = 33
        const val NODE_DELETE_DEEP = 34
        const val NODE_PATH = 35
        val PALETTE = intArrayOf(
            Color.WHITE, Color.parseColor("#e9ecef"), Color.parseColor("#ffc9c9"), Color.parseColor("#ffd8a8"),
            Color.parseColor("#fff3bf"), Color.parseColor("#b2f2bb"), Color.parseColor("#a5d8ff"), Color.parseColor("#d0bfff")
        )
    }
}
