/*
 * Streamlens smoke test: AV_FRAME_DATA_SL_H264_ANALYSIS export
 *
 * Opens fixtures/h264_prog.264 with libavformat/libavcodec from the vendor
 * prefix, enables export_sl_analysis (and AV_CODEC_FLAG2_EXPORT_MVS for
 * stock motion-vector side data), decodes all frames, and:
 *
 *  - prints per-frame: frame number, pict_type, sl_analysis presence+size,
 *    MB counts (intra/inter/skip), QP min/max, nonzero MV count.
 *  - For frame 0 (IDR): asserts all MBs are intra, modes in valid range.
 *  - Prints count of AV_FRAME_DATA_MOTION_VECTORS and AV_FRAME_DATA_VIDEO_ENC_PARAMS.
 *  - Exits nonzero on assertion failure.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <assert.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/frame.h>
#include <libavutil/motion_vector.h>
#include <libavutil/opt.h>
#include <libavutil/sl_h264_analysis.h>

#define FIXTURE_PATH "../../fixtures/h264_prog.264"

static void die(const char *msg, int ret)
{
    char errbuf[128];
    av_strerror(ret, errbuf, sizeof(errbuf));
    fprintf(stderr, "FATAL: %s: %s\n", msg, errbuf);
    exit(1);
}

static void assert_fail(const char *expr, int frame, int mb)
{
    fprintf(stderr, "ASSERT FAILED at frame %d MB %d: %s\n", frame, mb, expr);
    exit(1);
}

#define CHECK(expr, frame, mb) do { \
    if (!(expr)) assert_fail(#expr, frame, mb); \
} while (0)

int main(void)
{
    AVFormatContext *fmt = NULL;
    AVCodecContext  *dec = NULL;
    AVPacket        *pkt = NULL;
    AVFrame         *frm = NULL;
    const AVCodec   *codec;
    int video_stream = -1;
    int ret;
    int frame_num = 0;
    /* Queue to correlate send_packet sizes with received frames (threads=1, in-order). */
    int pkt_size_queue[4096];
    int pkt_size_queue_head = 0, pkt_size_queue_tail = 0;
#define PKT_QUEUE_PUSH(sz) do { pkt_size_queue[pkt_size_queue_tail++ & 4095] = (sz); } while(0)
#define PKT_QUEUE_POP()    (pkt_size_queue_head < pkt_size_queue_tail ? \
                            pkt_size_queue[pkt_size_queue_head++ & 4095] : -1)

    ret = avformat_open_input(&fmt, FIXTURE_PATH, NULL, NULL);
    if (ret < 0) die("avformat_open_input", ret);

    ret = avformat_find_stream_info(fmt, NULL);
    if (ret < 0) die("avformat_find_stream_info", ret);

    for (unsigned i = 0; i < fmt->nb_streams; i++) {
        if (fmt->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_VIDEO) {
            video_stream = (int)i;
            break;
        }
    }
    if (video_stream < 0) { fprintf(stderr, "No video stream found\n"); return 1; }

    codec = avcodec_find_decoder(fmt->streams[video_stream]->codecpar->codec_id);
    if (!codec) { fprintf(stderr, "No decoder found\n"); return 1; }

    dec = avcodec_alloc_context3(codec);
    if (!dec) { fprintf(stderr, "avcodec_alloc_context3 failed\n"); return 1; }

    ret = avcodec_parameters_to_context(dec, fmt->streams[video_stream]->codecpar);
    if (ret < 0) die("avcodec_parameters_to_context", ret);

    /* threads=1 is required for export_sl_analysis */
    dec->thread_count = 1;

    /* Enable stock motion-vector export */
    dec->flags2 |= AV_CODEC_FLAG2_EXPORT_MVS;

    /* Enable enc-params export */
    dec->export_side_data |= AV_CODEC_EXPORT_DATA_VIDEO_ENC_PARAMS;

    /* Enable Streamlens per-MB analysis */
    ret = av_opt_set_int(dec, "export_sl_analysis", 1, AV_OPT_SEARCH_CHILDREN);
    if (ret < 0) die("av_opt_set export_sl_analysis", ret);

    ret = avcodec_open2(dec, codec, NULL);
    if (ret < 0) die("avcodec_open2", ret);

    pkt = av_packet_alloc();
    frm = av_frame_alloc();
    if (!pkt || !frm) { fprintf(stderr, "alloc failed\n"); return 1; }

    while (1) {
        ret = av_read_frame(fmt, pkt);
        if (ret == AVERROR_EOF) {
            /* flush */
            avcodec_send_packet(dec, NULL);
        } else if (ret < 0) {
            die("av_read_frame", ret);
        } else {
            if (pkt->stream_index != video_stream) {
                av_packet_unref(pkt);
                continue;
            }
            PKT_QUEUE_PUSH(pkt->size);
            ret = avcodec_send_packet(dec, pkt);
            av_packet_unref(pkt);
            if (ret < 0) die("avcodec_send_packet", ret);
        }

        while (1) {
            ret = avcodec_receive_frame(dec, frm);
            if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) break;
            if (ret < 0) die("avcodec_receive_frame", ret);

            /* --- Inspect AV_FRAME_DATA_SL_H264_ANALYSIS --- */
            const AVFrameSideData *sl_sd =
                av_frame_get_side_data(frm, AV_FRAME_DATA_SL_H264_ANALYSIS);
            const AVFrameSideData *mv_sd =
                av_frame_get_side_data(frm, AV_FRAME_DATA_MOTION_VECTORS);
            const AVFrameSideData *enc_sd =
                av_frame_get_side_data(frm, AV_FRAME_DATA_VIDEO_ENC_PARAMS);

            int sl_present = (sl_sd != NULL);
            size_t sl_size = sl_sd ? (size_t)sl_sd->size : 0;

            /* Count MBs by type, QP range, nonzero MVs */
            int n_intra = 0, n_inter = 0, n_skip = 0, n_other = 0;
            int qp_min = 255, qp_max = -1;
            long nz_mv = 0;
            long bits_nonzero = 0;
            uint64_t bits_sum = 0;
            uint32_t bits_min = UINT32_MAX, bits_max = 0;
            unsigned int nb_mb_total = 0;
            int pkt_size = PKT_QUEUE_POP(); /* coded packet size in bytes, -1 if unknown */

            if (sl_sd && sl_sd->size >= (int)sizeof(SLH264AnalysisHeader)) {
                const SLH264AnalysisHeader *hdr =
                    (const SLH264AnalysisHeader *)sl_sd->data;
                const SLH264MBRecord *recs =
                    (const SLH264MBRecord *)(sl_sd->data + sizeof(SLH264AnalysisHeader));
                unsigned int nb_mb = hdr->mb_width * hdr->mb_height;
                nb_mb_total = nb_mb;

                if (sl_sd->size < (int)(sizeof(SLH264AnalysisHeader) +
                                        nb_mb * sizeof(SLH264MBRecord))) {
                    fprintf(stderr, "Frame %d: side data too small!\n", frame_num);
                    return 1;
                }

                /* version check */
                if (hdr->version != SL_H264_ANALYSIS_VERSION) {
                    fprintf(stderr, "Frame %d: unexpected header version %u (want %u)\n",
                            frame_num, hdr->version, SL_H264_ANALYSIS_VERSION);
                    return 1;
                }

                for (unsigned int i = 0; i < nb_mb; i++) {
                    uint32_t mbt = recs[i].mb_type;
                    int q = recs[i].qp;
                    if (q < qp_min) qp_min = q;
                    if (q > qp_max) qp_max = q;

                    uint32_t mb_bits = recs[i].bits;
                    bits_sum += mb_bits;
                    if (mb_bits > 0) bits_nonzero++;
                    if (mb_bits < bits_min) bits_min = mb_bits;
                    if (mb_bits > bits_max) bits_max = mb_bits;

                    /* IS_INTRA: bits 0-2 (MB_TYPE_INTRA4x4|INTRA16x16|INTRA_PCM) */
                    if (mbt & 7)
                        n_intra++;
                    else if (mbt & (1 << 17)) /* MB_TYPE_SKIP = bit 17 */
                        n_skip++;
                    else
                        n_inter++;

                    /* Count nonzero MVs (list 0 only, to avoid false inter count) */
                    for (int b = 0; b < 16; b++) {
                        if (recs[i].mv[0][b][0] != 0 || recs[i].mv[0][b][1] != 0)
                            nz_mv++;
                    }

                    /* Frame 0 (IDR) assertions */
                    if (frame_num == 0) {
                        CHECK(mbt & 7, frame_num, (int)i); /* must be intra */
                        if (mbt & 1) { /* IS_INTRA4x4 */
                            for (int m = 0; m < 16; m++) {
                                /* 0-8: standard H.264 modes; 9-11: FFmpeg
                                 * DC variants (LEFT_DC/TOP_DC/DC_128_PRED)
                                 * for border MBs with missing neighbors. */
                                int8_t mode = recs[i].intra4x4_pred_modes[m];
                                CHECK(mode >= 0 && mode <= 11, frame_num, (int)i);
                            }
                        }
                        if (mbt & 2) { /* IS_INTRA16x16 */
                            uint8_t m16 = recs[i].intra16x16_pred_mode;
                            /* 0-3 standard H.264 modes; 4-9 FFmpeg internal
                             * DC variants (LEFT_DC/TOP_DC/DC_128/ALZHEIMER). */
                            CHECK(m16 <= 9, frame_num, (int)i);
                        }
                        /* chroma pred mode: 0-3 standard; 4-9 FFmpeg internal */
                        CHECK(recs[i].chroma_pred_mode <= 9, frame_num, (int)i);
                    }
                }

                /* Frame 0: assert header version and bits plausibility */
                if (frame_num == 0) {
                    CHECK(hdr->version == SL_H264_ANALYSIS_VERSION, frame_num, -1);

                    /* >70% of MBs must have bits > 0 (IDR frame, all coded).
                     * CABAC approximation via log2(low) has ~1-bit precision; highly
                     * predictable MBs (e.g. smooth INTRA16x16 at low QP) can measure 0. */
                    long threshold = (long)(nb_mb * 70) / 100;
                    if (bits_nonzero < threshold) {
                        fprintf(stderr,
                                "Frame 0: only %ld/%u MBs have bits>0 (need >70%%)\n",
                                bits_nonzero, nb_mb);
                        return 1;
                    }

                    /* sum(bits)/8 should be within [0.5x, 1.2x] of coded frame size */
                    if (pkt_size > 0) {
                        double bits_bytes = (double)bits_sum / 8.0;
                        double lo = 0.5 * (double)pkt_size;
                        double hi = 1.2 * (double)pkt_size;
                        if (bits_bytes < lo || bits_bytes > hi) {
                            fprintf(stderr,
                                    "Frame 0: sum(bits)/8 = %.0f outside [%.0f, %.0f] "
                                    "(pkt_size=%ld)\n",
                                    bits_bytes, lo, hi, (long)pkt_size);
                            return 1;
                        }
                        printf("Frame 0 bits check: sum=%llu  sum/8=%.0f  "
                               "pkt_size=%ld  ratio=%.2f  min=%u avg=%llu max=%u\n",
                               (unsigned long long)bits_sum, bits_bytes,
                               (long)pkt_size,
                               bits_bytes / (double)pkt_size,
                               bits_min,
                               nb_mb > 0 ? bits_sum / nb_mb : 0,
                               bits_max);
                    }
                }
            }

            printf("frame %4d  pict_type=%c  sl_analysis=%s(%zu B)  "
                   "intra=%d inter=%d skip=%d  QP[%d..%d]  nzmv=%ld  "
                   "bits_nz=%ld  enc_params=%s  mv_sidedata=%s\n",
                   frame_num,
                   av_get_picture_type_char(frm->pict_type),
                   sl_present ? "yes" : "no", sl_size,
                   n_intra, n_inter, n_skip,
                   qp_min == 255 ? -1 : qp_min,
                   qp_max == -1  ? -1 : qp_max,
                   nz_mv,
                   bits_nonzero,
                   enc_sd ? "yes" : "no",
                   mv_sd  ? "yes" : "no");

            av_frame_unref(frm);
            frame_num++;
        }

        if (ret == AVERROR_EOF) break;
    }

    printf("\nDecoded %d frames. All assertions passed.\n", frame_num);

    av_frame_free(&frm);
    av_packet_free(&pkt);
    avcodec_free_context(&dec);
    avformat_close_input(&fmt);
    return 0;
}
