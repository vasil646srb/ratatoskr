package dev.vasil.yggproxy

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.ImageReader
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Size
import android.view.Gravity
import android.view.Surface
import android.view.TextureView
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowInsets
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import com.google.zxing.PlanarYUVLuminanceSource

/** Camera2 QR scanner: returns the decoded text as [EXTRA_TEXT]. */
class ScanActivity : Activity() {

    companion object {
        const val EXTRA_TEXT = "text"
        private const val REQ_CAMERA = 1
    }

    private lateinit var texture: TextureView
    private var thread: HandlerThread? = null
    private var bg: Handler? = null
    private var camera: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    @Volatile private var done = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val d = resources.displayMetrics.density
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(Color.BLACK) }
        texture = TextureView(this)
        val frame = FrameLayout(this)
        frame.addView(texture, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        root.addView(frame, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        root.addView(TextView(this).apply {
            text = "Наведите камеру на QR-код сервера Ratatoskr"
            setTextColor(Color.WHITE); textSize = 15f; gravity = Gravity.CENTER
            setPadding((16 * d).toInt(), (12 * d).toInt(), (16 * d).toInt(), 0)
        }, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        root.addView(Button(this, null, android.R.attr.borderlessButtonStyle).apply {
            text = "Отмена"; isAllCaps = false; setTextColor(Color.WHITE); textSize = 16f
            setOnClickListener { finish() }
        }, LinearLayout.LayoutParams(MATCH_PARENT, (56 * d).toInt()))
        setContentView(root)
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false)
            window.isNavigationBarContrastEnforced = false
            root.setOnApplyWindowInsetsListener { v, ins ->
                val b = ins.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
                v.setPadding(b.left, b.top, b.right, b.bottom)
                WindowInsets.CONSUMED
            }
        }
        // Keep the 3:4 preview undistorted: fit the TextureView into the frame.
        frame.addOnLayoutChangeListener { _, l, t, r, b, _, _, _, _ ->
            val w = r - l; val h = b - t
            val (pw, ph) = if (w * 4 / 3 <= h) w to w * 4 / 3 else h * 3 / 4 to h
            val lp = texture.layoutParams as FrameLayout.LayoutParams
            if (lp.width != pw || lp.height != ph) {
                texture.layoutParams = FrameLayout.LayoutParams(pw, ph, Gravity.CENTER)
            }
        }
    }

    override fun onResume() {
        super.onResume()
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.CAMERA), REQ_CAMERA)
            return
        }
        start()
    }

    override fun onPause() {
        stop()
        super.onPause()
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        if (requestCode != REQ_CAMERA) return
        if (grantResults.firstOrNull() != PackageManager.PERMISSION_GRANTED) {
            Toast.makeText(this, "Нужен доступ к камере для сканирования QR", Toast.LENGTH_LONG).show()
            finish()
        }
        // Granted: onResume runs again and starts the camera.
    }

    private fun start() {
        if (thread != null) return
        thread = HandlerThread("qr-scan").also { it.start() }
        bg = Handler(thread!!.looper)
        if (texture.isAvailable) openCamera()
        else texture.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(s: SurfaceTexture, w: Int, h: Int) = openCamera()
            override fun onSurfaceTextureSizeChanged(s: SurfaceTexture, w: Int, h: Int) {}
            override fun onSurfaceTextureDestroyed(s: SurfaceTexture) = true
            override fun onSurfaceTextureUpdated(s: SurfaceTexture) {}
        }
    }

    private fun stop() {
        try { session?.close() } catch (_: Exception) {}
        session = null
        try { camera?.close() } catch (_: Exception) {}
        camera = null
        try { reader?.close() } catch (_: Exception) {}
        reader = null
        thread?.quitSafely()
        thread = null
        bg = null
    }

    private fun fail(msg: String) = runOnUiThread {
        Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
        finish()
    }

    /** 4:3 size closest to `want` pixels wide. */
    private fun pick(sizes: Array<Size>, want: Int): Size =
        sizes.filter { it.width * 3 == it.height * 4 }.ifEmpty { sizes.toList() }
            .minByOrNull { kotlin.math.abs(it.width - want) }!!

    @Suppress("MissingPermission")
    private fun openCamera() {
        val handler = bg ?: return
        val cm = getSystemService(CAMERA_SERVICE) as CameraManager
        val id = try {
            cm.cameraIdList.firstOrNull { cm.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK }
                ?: cm.cameraIdList.firstOrNull()
        } catch (e: Exception) { null } ?: return fail("Камера недоступна")
        val map = cm.getCameraCharacteristics(id).get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: return fail("Камера недоступна")
        val previewSize = pick(map.getOutputSizes(SurfaceTexture::class.java), 1280)
        val scanSize = pick(map.getOutputSizes(ImageFormat.YUV_420_888), 1280)

        val r = ImageReader.newInstance(scanSize.width, scanSize.height, ImageFormat.YUV_420_888, 2)
        reader = r
        var buf = ByteArray(0)
        r.setOnImageAvailableListener({ ir ->
            val img = ir.acquireLatestImage() ?: return@setOnImageAvailableListener
            try {
                if (done) return@setOnImageAvailableListener
                val w = img.width; val h = img.height
                val plane = img.planes[0]
                val rowStride = plane.rowStride
                val src = plane.buffer
                if (buf.size != w * h) buf = ByteArray(w * h)
                for (y in 0 until h) { src.position(y * rowStride); src.get(buf, y * w, w) }
                val text = Qr.decode(PlanarYUVLuminanceSource(buf, w, h, 0, 0, w, h, false))
                if (text != null && !done) {
                    done = true
                    runOnUiThread {
                        setResult(RESULT_OK, Intent().putExtra(EXTRA_TEXT, text))
                        finish()
                    }
                }
            } catch (_: Exception) {
            } finally { img.close() }
        }, handler)

        try {
            cm.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(cam: CameraDevice) {
                    camera = cam
                    val st = texture.surfaceTexture ?: return
                    st.setDefaultBufferSize(previewSize.width, previewSize.height)
                    val preview = Surface(st)
                    @Suppress("DEPRECATION")
                    cam.createCaptureSession(listOf(preview, r.surface), object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            if (camera == null) return
                            session = s
                            val req = cam.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                                addTarget(preview); addTarget(r.surface)
                                set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE)
                            }.build()
                            try { s.setRepeatingRequest(req, null, handler) } catch (_: Exception) {}
                        }
                        override fun onConfigureFailed(s: CameraCaptureSession) = fail("Не удалось запустить камеру")
                    }, handler)
                }
                override fun onDisconnected(cam: CameraDevice) { cam.close(); camera = null }
                override fun onError(cam: CameraDevice, error: Int) { cam.close(); camera = null; fail("Ошибка камеры ($error)") }
            }, handler)
        } catch (e: Exception) {
            fail("Камера: ${e.message}")
        }
    }
}
