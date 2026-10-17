package com.skadi.core

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** SKADI-T-0637: films and episodes on the phone. */
class OfflineVideoTest {
    private val root = Files.createTempDirectory("videos").toFile()
    private val store = OfflineVideoStore(root)

    private fun episode(sid: String, eid: String, season: Int, number: Int) = OfflineVideo(
        key = OfflineVideo.episodeKey(sid, eid), kind = "episode", parentId = sid, itemId = eid,
        title = "Show $sid", subtitle = "S%02dE%02d · Pilot".format(season, number),
        season = season, number = number, sizeBytes = 10,
    )

    private fun complete(v: OfflineVideo) {
        store.saveMeta(v)
        store.videoFile(v.key).writeBytes(ByteArray(v.sizeBytes.toInt()))
    }

    @Test
    fun `paths sit under videos with a safe directory name`() {
        val key = OfflineVideo.movieKey("m-1", "e/2")
        assertEquals("movie_m-1_e_2", OfflineVideoStore.dirName(key))
        assertEquals(File(root, "videos/movie_m-1_e_2/video"), store.videoFile(key))
        assertEquals(File(root, "videos/movie_m-1_e_2/video.part"), store.partFile(key))
        assertEquals(File(root, "videos/movie_m-1_e_2/poster.jpg"), store.posterFile(key))
        val t = SubtitleTrack(index = 3, label = "English", format = "srt")
        assertEquals(File(root, "videos/movie_m-1_e_2/subs/3.srt"), store.subtitleFile(key, t))
    }

    @Test
    fun `the key matches the video progress key`() {
        assertEquals("movie:m:e", OfflineVideo.movieKey("m", "e"))
        assertEquals("episode:s:e", OfflineVideo.episodeKey("s", "e"))
        val m = Movie(id = "m", title = "Heat", year = 1995)
        assertEquals("movie:m:e", VideoDownloadRequest.movie(m, "e").key)
        val s = Series(id = "s", title = "Show")
        val e = Episode(id = "e", season = 1, number = 2, title = "Two")
        val r = VideoDownloadRequest.episode(s, e)
        assertEquals("episode:s:e", r.key)
        assertEquals("S01E02 · Two", r.subtitle)
    }

    @Test
    fun `meta without the video is not a download`() {
        val v = episode("s", "e1", 1, 1)
        store.saveMeta(v)
        assertFalse(store.isDownloaded(v.key))
        assertNull(store.meta(v.key))
        assertTrue(store.list().isEmpty())
        store.videoFile(v.key).writeBytes(ByteArray(10))
        assertTrue(store.isDownloaded(v.key))
        assertEquals(v, store.meta(v.key))
    }

    @Test
    fun `a part file alone is not a download but counts as used space`() {
        val key = OfflineVideo.movieKey("m", "e")
        store.partFile(key).apply { parentFile!!.mkdirs(); writeBytes(ByteArray(1234)) }
        assertFalse(store.isDownloaded(key))
        assertEquals(1234L, store.partialBytes(key))
        assertEquals(1234L, store.partialBytesTotal())
    }

    @Test
    fun `list puts films first, then episodes in order`() {
        complete(episode("s", "e3", 2, 1))
        complete(episode("s", "e1", 1, 1))
        complete(OfflineVideo(key = "movie:m:e", kind = "movie", parentId = "m", itemId = "e", title = "Zulu", sizeBytes = 5))
        assertEquals(listOf("movie:m:e", "episode:s:e1", "episode:s:e3"), store.list().map { it.key })
    }

    @Test
    fun `delete removes everything including the part`() {
        val v = episode("s", "e1", 1, 1)
        complete(v)
        store.partFile(v.key).writeBytes(ByteArray(3))
        store.delete(v.key)
        assertFalse(store.dir(v.key).exists())
        assertTrue(store.list().isEmpty())
    }

    @Test
    fun `next downloaded episode skips gaps and specials`() {
        val vs = listOf(
            episode("s", "a", 1, 1), episode("s", "c", 1, 4), episode("s", "d", 2, 1),
            episode("s", "x", 0, 1), episode("t", "y", 1, 2),
        )
        assertEquals("c", nextDownloadedEpisode(vs, "s", 1, 1)?.itemId)
        assertEquals("d", nextDownloadedEpisode(vs, "s", 1, 4)?.itemId)
        assertNull(nextDownloadedEpisode(vs, "s", 2, 1))
        assertEquals("a", nextDownloadedEpisode(vs, "s", 0, 1)?.itemId)
    }

    @Test
    fun `storage use adds books, videos and unfinished downloads`() {
        val use = StorageUse.of(
            books = listOf(OfflineBook(bookId = "b", fileId = "f", title = "B", sizeBytes = 300)),
            videos = listOf(episode("s", "a", 1, 1).copy(sizeBytes = 2_000)),
            partialBytes = 50,
        )
        assertEquals(300L, use.books)
        assertEquals(2_000L, use.videos)
        assertEquals(2_350L, use.total)
    }

    @Test
    fun `player titles`() {
        assertEquals("Show s · S01E02", episode("s", "e", 1, 2).playerTitle)
        val m = OfflineVideo(key = "movie:m:e", kind = "movie", parentId = "m", itemId = "e", title = "Heat", subtitle = "1995")
        assertEquals("Heat (1995)", m.playerTitle)
    }

    @Test
    fun `the downloader saves video, subtitles, markers and poster, meta last`() = runBlocking {
        val server = FakeServer()
        val data = bytes(700_000)
        val base = "/api/v1/series/s/episodes/e"
        server.files["$base/video"] = data
        server.texts["$base/subtitles"] = """[{"index":0,"language":"en","label":"English","format":"srt"}]"""
        server.files["$base/subtitles/0"] = "1\n00:00:01,000 --> 00:00:02,000\nHi\n".toByteArray()
        server.texts["$base/markers"] = """{"intro_start":1.0,"intro_end":30.0}"""
        server.files["/poster.jpg"] = bytes(100)
        val api = SkadiApi("http://skadi.test", "tok", server.client)
        val req = VideoDownloadRequest(
            kind = "episode", parentId = "s", itemId = "e", title = "Show",
            subtitle = "S01E01 · Pilot", season = 1, number = 1, posterUrl = "http://skadi.test/poster.jpg",
        )
        val r = VideoDownloader(api, store, server.client, now = { 42L }).download(req) {}
        assertTrue(r.exceptionOrNull()?.toString(), r.isSuccess)
        val meta = store.meta("episode:s:e")!!
        assertEquals(700_000L, meta.sizeBytes)
        assertEquals(42L, meta.downloadedAt)
        assertEquals(listOf("English"), meta.subtitles.map { it.label })
        assertEquals(30.0, meta.markers.introEnd!!, 0.0)
        assertArrayEquals(data, store.videoFile("episode:s:e").readBytes())
        assertTrue(store.subtitleFile("episode:s:e", meta.subtitles[0]).isFile)
        assertEquals(100L, store.posterFile("episode:s:e").length())
        val video = server.requests.first { it.url.encodedPath == "$base/video" }
        assertEquals("Bearer tok", video.header("Authorization"))
    }

    @Test
    fun `a failed video fetch writes no meta`() = runBlocking {
        val server = FakeServer()
        val api = SkadiApi("http://skadi.test", "tok", server.client)
        val req = VideoDownloadRequest(kind = "movie", parentId = "m", itemId = "e", title = "Heat")
        val r = VideoDownloader(api, store, server.client).download(req) {}
        assertTrue(r.isFailure)
        assertFalse(store.isDownloaded(req.key))
    }
}
