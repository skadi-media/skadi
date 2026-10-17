package com.skadi.core

import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import okio.BufferedSource

/**
 * An in-process server for downloader tests: an OkHttp interceptor that answers
 * by path, with Range handled like the daemon's file routes. No sockets.
 */
class FakeServer {
    val files = mutableMapOf<String, ByteArray>()
    val texts = mutableMapOf<String, String>()
    val requests = mutableListOf<okhttp3.Request>()

    /** Ignore Range and always send the whole file with 200. */
    var ignoreRange = false

    /** Answer a Range with this start instead of the asked one. */
    var wrongStart: Long? = null

    /** Cut a body after this many bytes (a dropped connection). */
    var cutAfter: Int? = null

    /** Answer the next N video requests with 416. */
    var force416 = 0

    val client: OkHttpClient = OkHttpClient.Builder().addInterceptor(Interceptor { chain -> answer(chain) }).build()

    private fun answer(chain: Interceptor.Chain): Response {
        val req = chain.request()
        requests += req
        val path = req.url.encodedPath
        val b = Response.Builder().request(req).protocol(Protocol.HTTP_1_1)
        texts[path]?.let {
            return b.code(200).message("OK").body(it.toResponseBody("application/json".toMediaType())).build()
        }
        val data = files[path] ?: return b.code(404).message("Not Found").body("".toResponseBody()).build()
        if (force416 > 0) {
            force416--
            return b.code(416).message("Range Not Satisfiable").body("".toResponseBody()).build()
        }
        val range = req.header("Range")
        if (range == null || ignoreRange) {
            return b.code(200).message("OK").header("Content-Length", "${data.size}")
                .body(body(data)).build()
        }
        val asked = Regex("bytes=(\\d+)-").find(range)!!.groupValues[1].toLong()
        if (asked >= data.size) {
            return b.code(416).message("Range Not Satisfiable").body("".toResponseBody()).build()
        }
        val start = wrongStart ?: asked
        val slice = data.copyOfRange(start.toInt(), data.size)
        return b.code(206).message("Partial Content")
            .header("Content-Range", "bytes $start-${data.size - 1}/${data.size}")
            .body(body(slice)).build()
    }

    /** The body, declaring its full length even when [cutAfter] ends it early. */
    private fun body(data: ByteArray): ResponseBody {
        val sent = cutAfter?.let { data.copyOfRange(0, minOf(it, data.size)) } ?: data
        return object : ResponseBody() {
            override fun contentType() = "video/mp4".toMediaType()
            override fun contentLength() = data.size.toLong()
            override fun source(): BufferedSource = Buffer().write(sent)
        }
    }
}

fun bytes(n: Int): ByteArray = ByteArray(n) { (it % 251).toByte() }
