package dev.istmo.plugins.notifications

/**
 * The port of `src/validate.rs`: the limits every host checks before a
 * platform sees a notification, with the same field paths and problems
 * (`Invalid(InvalidField("actions[2].title", TooLong))`), so a Kotlin or
 * Rust caller gets the answer the desktop host gives. Plain JVM code;
 * the Rust tests are mirrored in `tests/native/android/NativeChecks.kt`.
 *
 * Characters are counted as Unicode scalar values (Rust `chars()`), not
 * UTF-16 units; control characters are category Cc (Rust
 * `char::is_control`).
 */
object Validate {
    const val MAX_ID_LEN = 64
    const val MAX_TITLE_CHARS = 256
    const val MAX_BODY_CHARS = 4096
    const val MAX_STATUS_CHARS = 64
    const val MAX_LABEL_CHARS = 128
    const val MAX_ACTIONS = 16
    const val MAX_CHOICES = 10
    const val MAX_INBOX_LINES = 16
    const val MAX_URL_LEN = 2048
    const val MAX_USER_DATA_LEN = 3 * 1024
    const val MAX_IMAGE_BYTES = 2 * 1024 * 1024
    const val MAX_PROGRESS_MARKS = 32
    const val MAX_VIBRATION_STEPS = 32
    const val MIN_REPEAT_MS = 60_000uL
    const val MIN_SNOOZE_MS = 1_000uL
    const val MAX_SNOOZE_MS = 2_592_000_000uL // 30 days

    private const val RESERVED_ACTION_PREFIX = "istmo."
    /** `ActionId::CALL_*`: the reserved ids a [Style.Call] notification may carry. */
    private val CALL_ACTIONS = setOf("istmo.call.answer", "istmo.call.decline", "istmo.call.hang_up")

    /** A failed check: the field path and its problem. */
    private class Failed(val field: InvalidField) : Exception(null, null, false, false)

    private fun fail(field: String, problem: Problem): Nothing = throw Failed(InvalidField(field, problem))

    private inline fun checked(block: () -> Unit): InvalidField? = try {
        block()
        null
    } catch (e: Failed) {
        e.field
    }

    // ---- Leaf rules ---------------------------------------------------

    private fun isControl(cp: Int): Boolean = Character.getType(cp) == Character.CONTROL.toInt()

    private fun isWhitespace(cp: Int): Boolean = Character.isWhitespace(cp) || Character.isSpaceChar(cp)

    /** At most [max] characters, no control characters (newline and tab pass when [multiline]). */
    private fun text(value: String, max: Int, multiline: Boolean): Problem? {
        var count = 0
        var i = 0
        while (i < value.length) {
            val cp = value.codePointAt(i)
            i += Character.charCount(cp)
            count++
            if (count > max) return Problem.TooLong
            if (isControl(cp) && !(multiline && (cp == '\n'.code || cp == '\t'.code))) return Problem.BadCharset
        }
        return null
    }

    private fun label(value: String, max: Int): Problem? {
        if (value.codePoints().allMatch(::isWhitespace)) return Problem.Empty
        return text(value, max, false)
    }

    private fun optional(value: String?, max: Int, multiline: Boolean): Problem? = value?.let { text(it, max, multiline) }

    private fun at(problem: Problem?, path: () -> String) {
        if (problem != null) fail(path(), problem)
    }

    private fun id(value: String): Problem? {
        if (value.isEmpty()) return Problem.Empty
        if (value.toByteArray(Charsets.UTF_8).size > MAX_ID_LEN) return Problem.TooLong
        val allowed = value.all { c -> (c in 'a'..'z') || (c in 'A'..'Z') || (c in '0'..'9') || c == '.' || c == '_' || c == ':' || c == '-' }
        return if (allowed) null else Problem.BadCharset
    }

    private fun checkId(field: String, value: String) = at(id(value)) { field }

    private fun url(value: String): Problem? {
        if (value.toByteArray(Charsets.UTF_8).size > MAX_URL_LEN) return Problem.TooLong
        val colon = value.indexOf(':')
        if (colon < 0) return Problem.BadUrl
        val scheme = value.substring(0, colon)
        val starts = scheme.isNotEmpty() && scheme[0].isAsciiLetter()
        val rest = scheme.drop(1).all { it.isAsciiLetter() || it in '0'..'9' || it == '+' || it == '-' || it == '.' }
        if (!starts || !rest || value.codePoints().anyMatch { isWhitespace(it) || isControl(it) }) return Problem.BadUrl
        return null
    }

    private fun Char.isAsciiLetter() = this in 'a'..'z' || this in 'A'..'Z'

    private fun image(value: NotificationImage): Problem? = when (value) {
        is NotificationImage.Png -> bytes(value.value.size)
        is NotificationImage.Jpeg -> bytes(value.value.size)
        is NotificationImage.File -> when {
            value.value.isEmpty() -> Problem.Empty
            !value.value.startsWith('/') -> Problem.NotAllowedHere
            else -> text(value.value, MAX_URL_LEN, false)
        }
        is NotificationImage.Named -> label(value.value, MAX_LABEL_CHARS)
    }

    private fun bytes(size: Int): Problem? = when {
        size == 0 -> Problem.Empty
        size > MAX_IMAGE_BYTES -> Problem.TooLarge
        else -> null
    }

    // ---- Composite rules ----------------------------------------------

    private fun participant(value: Participant, path: String) {
        at(label(value.key, MAX_LABEL_CHARS)) { "$path.key" }
        at(label(value.name, MAX_LABEL_CHARS)) { "$path.name" }
        value.avatar?.let { at(image(it)) { "$path.avatar" } }
    }

    private fun message(value: ChatMessage, path: String) {
        at(label(value.text, MAX_BODY_CHARS)) { "$path.text" }
        value.sender?.let { participant(it, "$path.sender") }
        value.image?.let { at(image(it)) { "$path.image" } }
    }

    private fun style(value: Style, path: String) {
        when (value) {
            is Style.Plain -> {}
            is Style.BigText -> at(text(value.value, MAX_BODY_CHARS, true)) { path }
            is Style.Inbox -> {
                if (value.value.size > MAX_INBOX_LINES) fail(path, Problem.TooLong)
                value.value.forEachIndexed { i, line -> at(text(line, MAX_TITLE_CHARS, false)) { "$path[$i]" } }
            }
            is Style.Picture -> at(image(value.value)) { path }
            is Style.Messaging -> {
                participant(value.value.user, "$path.user")
                at(optional(value.value.title, MAX_TITLE_CHARS, false)) { "$path.title" }
                value.value.messages.forEachIndexed { i, m -> message(m, "$path.messages[$i]") }
            }
            is Style.Call -> participant(value.value.caller, "$path.caller")
        }
    }

    private fun progress(value: Progress, path: String) {
        value.value?.let { f -> if (!(f.isFinite() && f >= 0f && f <= 1f)) fail("$path.value", Problem.OutOfRange) }
        at(optional(value.label, MAX_LABEL_CHARS, false)) { "$path.label" }
        if (value.segments.size > MAX_PROGRESS_MARKS) fail("$path.segments", Problem.TooLong)
        if (value.points.size > MAX_PROGRESS_MARKS) fail("$path.points", Problem.TooLong)
    }

    private fun choices(values: List<String>, path: String) {
        if (values.size > MAX_CHOICES) fail(path, Problem.TooLong)
        val seen = HashSet<String>(values.size)
        values.forEachIndexed { i, choice ->
            at(label(choice, MAX_LABEL_CHARS)) { "$path[$i]" }
            if (!seen.add(choice)) fail("$path[$i]", Problem.Duplicate)
        }
    }

    private fun actions(values: List<Action>, isCall: Boolean) {
        if (values.size > MAX_ACTIONS) fail("actions", Problem.TooLong)
        val seen = HashSet<String>(values.size)
        values.forEachIndexed { i, action ->
            val key = action.id.value
            at(id(key)) { "actions[$i].id" }
            if (key.startsWith(RESERVED_ACTION_PREFIX) && !(isCall && key in CALL_ACTIONS)) fail("actions[$i].id", Problem.Reserved)
            if (!seen.add(key)) fail("actions[$i].id", Problem.Duplicate)
            at(label(action.title, MAX_LABEL_CHARS)) { "actions[$i].title" }
            action.url?.let { at(url(it)) { "actions[$i].url" } }
            action.icon?.let { at(image(it)) { "actions[$i].icon" } }
            when (val kind = action.kind) {
                is ActionKind.Button -> {}
                is ActionKind.Reply -> {
                    at(optional(kind.value.placeholder, MAX_LABEL_CHARS, false)) { "actions[$i].placeholder" }
                    at(optional(kind.value.submitTitle, MAX_LABEL_CHARS, false)) { "actions[$i].submit_title" }
                    choices(kind.value.suggestions, "actions[$i].suggestions")
                }
                is ActionKind.Select -> {
                    if (kind.value.isEmpty()) fail("actions[$i].choices", Problem.Empty)
                    choices(kind.value, "actions[$i].choices")
                }
                is ActionKind.Snooze -> if (kind.value < MIN_SNOOZE_MS || kind.value > MAX_SNOOZE_MS) fail("actions[$i].snooze", Problem.OutOfRange)
            }
        }
    }

    private fun hints(values: List<LinuxHint>) {
        values.forEachIndexed { i, hint ->
            if (hint.key.isEmpty()) fail("linux_hints[$i].key", Problem.Empty)
            if (hint.key.length > 64 || hint.key.any { it.code > 0x7f }) fail("linux_hints[$i].key", Problem.BadCharset)
            val size = when (val v = hint.value) {
                is HintValue.Bool, is HintValue.Number -> 0
                is HintValue.Text -> v.value.toByteArray(Charsets.UTF_8).size
                is HintValue.Bytes -> v.value.size
            }
            if (size > MAX_IMAGE_BYTES) fail("linux_hints[$i].value", Problem.TooLarge)
        }
    }

    private fun userData(value: String?) {
        if (value != null && value.toByteArray(Charsets.UTF_8).size > MAX_USER_DATA_LEN) fail("user_data", Problem.TooLarge)
    }

    // ---- Entry points ---------------------------------------------------

    /** `Notification::validate`. */
    fun notification(n: Notification): InvalidField? = checked {
        checkId("id", n.id.value)
        n.channel?.let { checkId("channel", it.value) }
        at(label(n.title, MAX_TITLE_CHARS)) { "title" }
        at(optional(n.subtitle, MAX_TITLE_CHARS, false)) { "subtitle" }
        at(optional(n.body, MAX_BODY_CHARS, true)) { "body" }
        style(n.style, "style")
        n.icon?.let { at(image(it)) { "icon" } }
        actions(n.actions, n.style is Style.Call)
        n.url?.let { at(url(it)) { "url" } }
        userData(n.userData)
        n.group?.let { g ->
            checkId("group", g.id.value)
            at(optional(g.title, MAX_TITLE_CHARS, false)) { "group.title" }
            at(optional(g.summary, MAX_TITLE_CHARS, false)) { "group.summary" }
        }
        n.progress?.let { progress(it, "progress") }
        at(optional(n.statusText, MAX_STATUS_CHARS, false)) { "status_text" }
        if (n.timeoutMs == 0uL) fail("timeout_ms", Problem.OutOfRange)
        n.apple?.relevanceScore?.let { s -> if (!(s.isFinite() && s >= 0f && s <= 1f)) fail("apple.relevance_score", Problem.OutOfRange) }
        hints(n.linuxHints)
    }

    /** `NotificationPatch::validate`: the fields it sets, and none both set and cleared. */
    fun patch(p: NotificationPatch): InvalidField? = checked {
        p.title?.let { at(label(it, MAX_TITLE_CHARS)) { "title" } }
        at(optional(p.subtitle, MAX_TITLE_CHARS, false)) { "subtitle" }
        at(optional(p.body, MAX_BODY_CHARS, true)) { "body" }
        p.style?.let { style(it, "style") }
        p.icon?.let { at(image(it)) { "icon" } }
        p.actions?.let { actions(it, p.style == null || p.style is Style.Call) }
        p.progress?.let { progress(it, "progress") }
        at(optional(p.statusText, MAX_STATUS_CHARS, false)) { "status_text" }
        p.url?.let { at(url(it)) { "url" } }
        userData(p.userData)
        for (field in p.clear) {
            val (set, name) = when (field) {
                PatchField.Subtitle -> (p.subtitle != null) to "subtitle"
                PatchField.Body -> (p.body != null) to "body"
                PatchField.Icon -> (p.icon != null) to "icon"
                PatchField.Progress -> (p.progress != null) to "progress"
                PatchField.StatusText -> (p.statusText != null) to "status_text"
                PatchField.Count -> (p.count != null) to "count"
                PatchField.Url -> (p.url != null) to "url"
                PatchField.UserData -> (p.userData != null) to "user_data"
                PatchField.Actions -> (p.actions != null) to "actions"
            }
            if (set) fail(name, Problem.NotAllowedHere)
        }
    }

    /** `Progress::validate`. */
    fun progress(p: Progress): InvalidField? = checked { progress(p, "progress") }

    /** `ChatMessage::validate`. */
    fun message(m: ChatMessage): InvalidField? = checked { message(m, "message") }

    /** `Trigger::validate`. */
    fun trigger(t: Trigger): InvalidField? = when (t) {
        is Trigger.After, is Trigger.At -> null
        is Trigger.Every -> if (t.value < MIN_REPEAT_MS) InvalidField("trigger.every", Problem.OutOfRange) else null
        is Trigger.Calendar -> rule(t.value)
    }

    private fun maxDay(month: Int): Int = when (month) {
        2 -> 29
        4, 6, 9, 11 -> 30
        else -> 31
    }

    /** `CalendarRule::validate`. */
    fun rule(r: CalendarRule): InvalidField? = checked {
        if (r.hour > 23u) fail("trigger.hour", Problem.OutOfRange)
        if (r.minute > 59u) fail("trigger.minute", Problem.OutOfRange)
        r.month?.let { if (it < 1u || it > 12u) fail("trigger.month", Problem.OutOfRange) }
        r.dayOfMonth?.let { day ->
            val max = r.month?.let { maxDay(it.toInt()) } ?: 31
            if (day.toInt() !in 1..max) fail("trigger.day_of_month", Problem.OutOfRange)
        }
        val seen = HashSet<Weekday>()
        for (day in r.weekdays) if (!seen.add(day)) fail("trigger.weekdays", Problem.Duplicate)
        r.timeZone?.let { if (!CalendarRules.knownZone(it)) fail("trigger.time_zone", Problem.OutOfRange) }
    }

    /** `ChannelSpec::validate`. */
    fun channel(c: ChannelSpec): InvalidField? = checked {
        checkId("channel", c.id.value)
        at(label(c.name, MAX_LABEL_CHARS * 2)) { "name" }
        at(optional(c.description, MAX_BODY_CHARS, true)) { "description" }
        if ((c.vibration?.size ?: 0) > MAX_VIBRATION_STEPS) fail("vibration", Problem.TooLong)
        at(optional(c.group, MAX_LABEL_CHARS, false)) { "group" }
    }

    /** `set_channels`: the first invalid spec, its field prefixed `channels[i].`. */
    fun channels(specs: List<ChannelSpec>): InvalidField? {
        specs.forEachIndexed { i, spec -> channel(spec)?.let { return InvalidField("channels[$i].${it.field}", it.problem) } }
        return null
    }
}
