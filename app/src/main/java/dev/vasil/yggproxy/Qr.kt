package dev.vasil.yggproxy

import android.graphics.Bitmap
import android.graphics.Color
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.EncodeHintType
import com.google.zxing.LuminanceSource
import com.google.zxing.NotFoundException
import com.google.zxing.ChecksumException
import com.google.zxing.FormatException
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeReader
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel

/** QR codes for the server's quick-connect link (zxing core, no Android dependencies). */
object Qr {
    fun bitmap(text: String, size: Int): Bitmap {
        val hints = mapOf(EncodeHintType.MARGIN to 2, EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.M)
        val m = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, size, size, hints)
        val px = IntArray(m.width * m.height)
        for (y in 0 until m.height) for (x in 0 until m.width) px[y * m.width + x] = if (m.get(x, y)) Color.BLACK else Color.WHITE
        return Bitmap.createBitmap(px, m.width, m.height, Bitmap.Config.ARGB_8888)
    }

    private val reader = QRCodeReader()
    private val decodeHints = mapOf(DecodeHintType.TRY_HARDER to true)

    /** Decode a QR code from a luminance image, or null. Not thread-safe: one scanner thread. */
    fun decode(src: LuminanceSource): String? = try {
        reader.decode(BinaryBitmap(HybridBinarizer(src)), decodeHints).text
    } catch (_: NotFoundException) { null } catch (_: ChecksumException) { null } catch (_: FormatException) { null }
    finally { reader.reset() }
}
