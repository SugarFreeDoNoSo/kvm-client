package local.kvm.mobile

import android.app.Activity
import android.graphics.Color
import android.os.Handler
import android.os.Looper
import android.view.*
import android.view.inputmethod.*
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.appcompat.app.AppCompatActivity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONArray
import org.json.JSONObject
import org.webrtc.*
import java.nio.ByteBuffer
import kotlin.math.*

@InvokeArg
class ExecuteArgs {
    lateinit var action: String
    var session: String = ""
    var data: String = ""
    var x: Double = 0.0
    var y: Double = 0.0
    var width: Double = 0.0
    var height: Double = 0.0
    var visible: Boolean = false
}

@TauriPlugin
class KvmPlugin(private val activity: Activity) : Plugin(activity) {
    private val ui = Handler(Looper.getMainLooper())
    private var web: WebView? = null
    private var surface: InputSurface? = null
    private var renderer: SurfaceViewRenderer? = null
    private var egl: EglBase? = null
    private var factory: PeerConnectionFactory? = null
    private var peer: PeerConnection? = null
    private var rpc: DataChannel? = null
    private var session = ""
    private var answered = false
    private val pendingIce = mutableListOf<IceCandidate>()
    private val remoteIce = mutableListOf<IceCandidate>()
    private val keys = java.util.ArrayDeque<Pair<Int, Int>>()
    private var typing = false
    private var buttons = 0
    private var dx = 0.0
    private var dy = 0.0
    private var framePending = false
    private val moveFrame = Runnable { framePending = false; flushMotion() }
    private val releaseClick = Runnable { buttons = 0; mouse(0, 0) }
    private val keyboardStep = Runnable { nextKey() }
    private val keyboardRelease = Runnable {
        report("keyboardReport", JSONObject().put("keys", JSONArray()).put("modifier", 0))
        ui.postDelayed(keyboardStep, 20)
    }

    override fun load(webView: WebView) { web = webView }
    override fun onPause(activity: AppCompatActivity) { releaseInput(); hideKeyboard(); event("keyboardHidden") }
    override fun onDestroy(activity: AppCompatActivity) { close() }

    private fun event(kind: String, data: String = "", id: String = session) {
        val payload = JSObject().put("session", id).put("kind", kind).put("data", data)
        trigger("session", JSObject(payload.toString()))
    }

    @Command
    fun execute(invoke: Invoke) {
        val args = invoke.parseArgs(ExecuteArgs::class.java)
        activity.runOnUiThread {
            try {
                if (args.action == "start") {
                    close()
                    require(args.session.isNotEmpty() && args.session.length <= 64)
                    session = args.session
                    start()
                } else {
                    require(args.session == session && session.isNotEmpty()) { "Sesión cerrada" }
                    when (args.action) {
                        "offer" -> offer()
                        "answer" -> answer(args.data)
                        "ice" -> addIce(args.data)
                        "viewport" -> viewport(args)
                        "keyboard" -> {
                            if (args.visible) {
                                surface?.requestFocus()
                                (activity.getSystemService(Activity.INPUT_METHOD_SERVICE) as InputMethodManager)
                                    .showSoftInput(surface, InputMethodManager.SHOW_IMPLICIT)
                            } else hideKeyboard()
                        }
                        "key" -> { val value = JSONObject(args.data); key(value.getInt("key"), value.optInt("modifier")) }
                        "release" -> releaseInput()
                        "close" -> close()
                        else -> error("Acción no permitida")
                    }
                }
                invoke.resolve()
            } catch (error: Exception) { invoke.reject(error.message ?: "Error nativo Android") }
        }
    }

    private fun start() {
        PeerConnectionFactory.initialize(PeerConnectionFactory.InitializationOptions.builder(activity.applicationContext).createInitializationOptions())
        val context = EglBase.create().also { egl = it }
        factory = PeerConnectionFactory.builder()
            .setVideoDecoderFactory(DefaultVideoDecoderFactory(context.eglBaseContext))
            .createPeerConnectionFactory()
        val view = InputSurface().also { surface = it }
        val render = SurfaceViewRenderer(activity).also { renderer = it }
        render.init(context.eglBaseContext, null)
        render.setEnableHardwareScaler(true)
        render.setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FIT)
        view.addView(render, FrameLayout.LayoutParams(-1, -1))
        view.visibility = View.GONE
        (activity.findViewById<View>(android.R.id.content) as FrameLayout).addView(view, FrameLayout.LayoutParams(1, 1))
        val id = session
        val config = PeerConnection.RTCConfiguration(emptyList()).apply { sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN }
        peer = factory!!.createPeerConnection(config, object : PeerConnection.Observer {
            override fun onSignalingChange(state: PeerConnection.SignalingState) {}
            override fun onIceConnectionChange(state: PeerConnection.IceConnectionState) {
                if (state == PeerConnection.IceConnectionState.FAILED || state == PeerConnection.IceConnectionState.DISCONNECTED) ui.post { if (session == id) event("error", "Se perdió la conexión WebRTC", id) }
            }
            override fun onIceConnectionReceivingChange(receiving: Boolean) {}
            override fun onIceGatheringChange(state: PeerConnection.IceGatheringState) {}
            override fun onIceCandidate(candidate: IceCandidate) { ui.post { if (session == id) { if (answered) emitIce(candidate) else pendingIce.add(candidate) } } }
            override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) {}
            override fun onAddStream(stream: MediaStream) {}
            override fun onRemoveStream(stream: MediaStream) {}
            override fun onDataChannel(channel: DataChannel) {}
            override fun onRenegotiationNeeded() {}
            override fun onAddTrack(receiver: RtpReceiver, streams: Array<out MediaStream>) {
                val track = receiver.track()
                if (track is VideoTrack) track.addSink(render)
            }
        }) ?: error("No se pudo crear WebRTC")
        peer!!.addTransceiver(MediaStreamTrack.MediaType.MEDIA_TYPE_VIDEO, RtpTransceiver.RtpTransceiverInit(RtpTransceiver.RtpTransceiverDirection.RECV_ONLY))
        peer!!.addTransceiver(MediaStreamTrack.MediaType.MEDIA_TYPE_AUDIO, RtpTransceiver.RtpTransceiverInit(RtpTransceiver.RtpTransceiverDirection.RECV_ONLY))
        rpc = peer!!.createDataChannel("rpc", DataChannel.Init().apply { ordered = true }).also { channel ->
            channel.registerObserver(object : DataChannel.Observer {
                override fun onBufferedAmountChange(previous: Long) {}
                override fun onMessage(buffer: DataChannel.Buffer) {}
                override fun onStateChange() { ui.post {
                    if (session == id) when (channel.state()) {
                        DataChannel.State.OPEN -> event("ready", id = id)
                        DataChannel.State.CLOSED -> event("error", "Se cerró el canal de control", id)
                        else -> {}
                    }
                } }
            })
        }
    }

    private fun sdpObserver(success: (SessionDescription?) -> Unit): SdpObserver {
        val id = session
        return object : SdpObserver {
            override fun onCreateSuccess(sdp: SessionDescription) { ui.post { if (session == id) success(sdp) } }
            override fun onSetSuccess() { ui.post { if (session == id) success(null) } }
            override fun onCreateFailure(error: String) { ui.post { if (session == id) event("error", error, id) } }
            override fun onSetFailure(error: String) { ui.post { if (session == id) event("error", error, id) } }
        }
    }

    private fun offer() {
        peer?.createOffer(sdpObserver { description ->
            if (description != null) peer?.setLocalDescription(sdpObserver {
                event("offer", description.description)
            }, description)
        }, MediaConstraints())
    }

    private fun answer(data: String) {
        val value = JSONObject(data)
        require(value.getString("type") == "answer")
        peer?.setRemoteDescription(sdpObserver {
            answered = true
            remoteIce.forEach { peer?.addIceCandidate(it) }; remoteIce.clear()
            pendingIce.forEach { emitIce(it) }; pendingIce.clear()
        }, SessionDescription(SessionDescription.Type.ANSWER, value.getString("sdp")))
    }

    private fun emitIce(candidate: IceCandidate) {
        event("ice", JSONObject().put("candidate", candidate.sdp).put("sdpMid", candidate.sdpMid).put("sdpMLineIndex", candidate.sdpMLineIndex).toString())
    }

    private fun addIce(data: String) {
        val value = JSONObject(data)
        val candidate = IceCandidate(value.optString("sdpMid", "0"), value.optInt("sdpMLineIndex", 0), value.getString("candidate"))
        if (answered) peer?.addIceCandidate(candidate) else remoteIce.add(candidate)
    }

    private fun viewport(args: ExecuteArgs) {
        val view = surface ?: return
        val scale = activity.resources.displayMetrics.density
        val webLocation = IntArray(2); web?.getLocationOnScreen(webLocation)
        val parentLocation = IntArray(2); (view.parent as View).getLocationOnScreen(parentLocation)
        view.layoutParams = FrameLayout.LayoutParams(max(1, (args.width * scale).roundToInt()), max(1, (args.height * scale).roundToInt())).apply {
            leftMargin = webLocation[0] - parentLocation[0] + (args.x * scale).roundToInt()
            topMargin = webLocation[1] - parentLocation[1] + (args.y * scale).roundToInt()
        }
        if (!args.visible) { releaseInput(); hideKeyboard() }
        view.visibility = if (args.visible) View.VISIBLE else View.GONE
    }

    private fun report(method: String, params: JSONObject): Boolean {
        val channel = rpc ?: return false
        if (channel.state() != DataChannel.State.OPEN) return false
        if (channel.bufferedAmount() > 65536) { event("error", "Canal de entrada congestionado. Vuelve a conectar."); return false }
        val bytes = JSONObject().put("jsonrpc", "2.0").put("method", method).put("params", params).toString().toByteArray(Charsets.UTF_8)
        val sent = channel.send(DataChannel.Buffer(ByteBuffer.wrap(bytes), false))
        if (!sent) event("error", "No se pudo enviar la entrada al KVM")
        return sent
    }

    private fun mouse(x: Int, y: Int) = report("relMouseReport", JSONObject().put("dx", x).put("dy", y).put("buttons", buttons))
    private fun move(x: Double, y: Double) {
        dx = (dx + x).coerceIn(-1000000.0, 1000000.0); dy = (dy + y).coerceIn(-1000000.0, 1000000.0)
        if (!framePending) { framePending = true; ui.postDelayed(moveFrame, 8) }
    }
    private fun flushMotion() {
        while (abs(dx) >= 1 || abs(dy) >= 1) {
            val x = dx.toInt().coerceIn(-127, 127); val y = dy.toInt().coerceIn(-127, 127)
            dx -= x; dy -= y
            if (!mouse(x, y)) { dx = 0.0; dy = 0.0; break }
        }
    }
    private fun click() {
        flushMotion(); buttons = 1
        if (mouse(0, 0)) ui.postDelayed(releaseClick, 25) else buttons = 0
    }
    private fun key(key: Int, modifier: Int = 0) {
        require(key in 4..231 && modifier in 0..255)
        if (keys.size >= 256) { event("error", "Demasiadas teclas pendientes"); return }
        keys.add(key to modifier)
        if (!typing) { typing = true; nextKey() }
    }
    private fun nextKey() {
        val next = keys.poll()
        if (next == null) { typing = false; return }
        if (report("keyboardReport", JSONObject().put("keys", JSONArray().put(next.first)).put("modifier", next.second))) ui.postDelayed(keyboardRelease, 20)
        else { keys.clear(); typing = false }
    }
    private fun commit(text: String) {
        for (char in text) {
            val mapped = HidKeyboard.character(char)
            if (mapped == null) event("warning", "El teclado remoto requiere distribución US; carácter no compatible: $char")
            else key(mapped.first, mapped.second)
        }
    }
    private fun hideKeyboard() {
        surface?.let { (activity.getSystemService(Activity.INPUT_METHOD_SERVICE) as InputMethodManager).hideSoftInputFromWindow(it.windowToken, 0); it.clearFocus() }
    }
    private fun releaseInput() {
        ui.removeCallbacks(moveFrame); ui.removeCallbacks(releaseClick); ui.removeCallbacks(keyboardStep); ui.removeCallbacks(keyboardRelease)
        framePending = false; dx = 0.0; dy = 0.0; buttons = 0; keys.clear(); typing = false
        surface?.trackpad?.cancel()
        report("keyboardReport", JSONObject().put("keys", JSONArray()).put("modifier", 0)); mouse(0, 0)
    }
    private fun close() {
        releaseInput(); hideKeyboard(); session = ""
        rpc?.unregisterObserver(); rpc?.close(); rpc?.dispose(); rpc = null
        peer?.close(); peer?.dispose(); peer = null
        renderer?.release(); renderer = null
        surface?.let { (it.parent as? FrameLayout)?.removeView(it) }; surface = null
        factory?.dispose(); factory = null; egl?.release(); egl = null
        answered = false; pendingIce.clear(); remoteIce.clear()
    }

    inner class InputSurface : FrameLayout(activity) {
        val trackpad = Trackpad(::move, ::click)
        init { isFocusable = true; isFocusableInTouchMode = true; setBackgroundColor(Color.BLACK) }
        override fun onInterceptTouchEvent(event: MotionEvent) = true
        override fun onTouchEvent(event: MotionEvent): Boolean {
            val density = resources.displayMetrics.density.toDouble()
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> trackpad.down(event.x / density, event.y / density, event.eventTime)
                MotionEvent.ACTION_POINTER_DOWN -> trackpad.block()
                MotionEvent.ACTION_MOVE -> if (event.pointerCount == 1) trackpad.move(event.x / density, event.y / density)
                MotionEvent.ACTION_UP -> trackpad.up(event.x / density, event.y / density, event.eventTime)
                MotionEvent.ACTION_CANCEL -> trackpad.cancel()
            }
            return true
        }
        override fun onWindowFocusChanged(hasWindowFocus: Boolean) { super.onWindowFocusChanged(hasWindowFocus); if (!hasWindowFocus) releaseInput() }
        override fun onCheckIsTextEditor() = true
        override fun onCreateInputConnection(info: EditorInfo): InputConnection {
            info.inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            info.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_ACTION_NONE
            return object : BaseInputConnection(this, false) {
                override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean { text?.let { commit(it.toString()) }; return true }
                override fun setComposingText(text: CharSequence?, newCursorPosition: Int) = true
                override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean { repeat(beforeLength.coerceIn(0, 64)) { key(42) }; return true }
                override fun sendKeyEvent(event: KeyEvent): Boolean { return handleKey(event) }
            }
        }
        private fun handleKey(event: KeyEvent): Boolean {
            if (event.action != KeyEvent.ACTION_DOWN) return true
            val special = mapOf(KeyEvent.KEYCODE_DEL to 42, KeyEvent.KEYCODE_ENTER to 40, KeyEvent.KEYCODE_TAB to 43, KeyEvent.KEYCODE_ESCAPE to 41, KeyEvent.KEYCODE_DPAD_UP to 82, KeyEvent.KEYCODE_DPAD_DOWN to 81, KeyEvent.KEYCODE_DPAD_LEFT to 80, KeyEvent.KEYCODE_DPAD_RIGHT to 79)
            special[event.keyCode]?.let { key(it); return true }
            if (event.unicodeChar != 0) commit(event.unicodeChar.toChar().toString())
            return true
        }
        override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean = handleKey(event)
    }
}
