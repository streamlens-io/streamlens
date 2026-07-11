/*
 * Streamlens differential thread smoke test: threads=1 vs threads=4 (FF_THREAD_FRAME).
 *
 * Decodes fixtures/h264_prog.264 twice:
 *   - pass 0: thread_count=1, thread_type=FF_THREAD_FRAME, export_sl_analysis=1, export_sl_stages=1
 *   - pass 1: thread_count=4, thread_type=FF_THREAD_FRAME, export_sl_analysis=1, export_sl_stages=1
 *
 * For each frame (all 75), compares:
 *   - pixel planes (Y, Cb, Cr): byte-exact
 *   - AV_FRAME_DATA_SL_H264_ANALYSIS blob: byte-exact
 *   - AV_FRAME_DATA_SL_H264_STAGES blob: byte-exact
 *
 * Exits nonzero on any mismatch.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/frame.h>
#include <libavutil/opt.h>
#include <libavutil/imgutils.h>
#include <libavutil/sl_h264_analysis.h>

#define FIXTURE_PATH "../../fixtures/h264_prog.264"
#define MAX_FRAMES 512

static void die(const char *msg, int ret)
{
    char errbuf[128];
    av_strerror(ret, errbuf, sizeof(errbuf));
    fprintf(stderr, "FATAL: %s: %s\n", msg, errbuf);
    exit(1);
}

typedef struct {
    int      frame_num;
    uint8_t *pixels[3];  /* Y, Cb, Cr — tightly packed (no stride padding) */
    int      pixel_w[3];
    int      pixel_h[3];
    uint8_t *analysis;
    size_t   analysis_size;
    uint8_t *stages;
    size_t   stages_size;
} FrameCapture;

static void free_frame_capture(FrameCapture *fc)
{
    for (int i = 0; i < 3; i++) {
        free(fc->pixels[i]);
        fc->pixels[i] = NULL;
    }
    free(fc->analysis);
    fc->analysis = NULL;
    free(fc->stages);
    fc->stages = NULL;
}

static void copy_plane(uint8_t **dst, int *w_out, int *h_out,
                       const uint8_t *src, int linesize, int w, int h)
{
    *w_out = w;
    *h_out = h;
    *dst = malloc((size_t)w * h);
    if (!*dst) { fprintf(stderr, "OOM\n"); exit(1); }
    for (int row = 0; row < h; row++)
        memcpy(*dst + row * w, src + row * linesize, w);
}

static void copy_side_data(uint8_t **dst, size_t *sz,
                           enum AVFrameSideDataType type, const AVFrame *f)
{
    const AVFrameSideData *sd = av_frame_get_side_data(f, type);
    if (!sd) { *dst = NULL; *sz = 0; return; }
    *dst = malloc(sd->size);
    if (!*dst) { fprintf(stderr, "OOM\n"); exit(1); }
    memcpy(*dst, sd->data, sd->size);
    *sz = sd->size;
}

static FrameCapture *decode_all(int nthreads, int *nframes_out)
{
    AVFormatContext *fmt = NULL;
    AVCodecContext  *dec = NULL;
    AVPacket        *pkt = NULL;
    AVFrame         *frm = NULL;
    const AVCodec   *codec;
    int video_stream = -1;
    int ret;

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
    if (video_stream < 0) { fprintf(stderr, "No video stream\n"); exit(1); }

    codec = avcodec_find_decoder(fmt->streams[video_stream]->codecpar->codec_id);
    if (!codec) { fprintf(stderr, "No decoder\n"); exit(1); }

    dec = avcodec_alloc_context3(codec);
    if (!dec) { fprintf(stderr, "avcodec_alloc_context3 failed\n"); exit(1); }

    ret = avcodec_parameters_to_context(dec, fmt->streams[video_stream]->codecpar);
    if (ret < 0) die("avcodec_parameters_to_context", ret);

    dec->thread_count = nthreads;
    dec->thread_type  = FF_THREAD_FRAME;

    ret = av_opt_set_int(dec, "export_sl_analysis", 1, AV_OPT_SEARCH_CHILDREN);
    if (ret < 0) die("av_opt_set export_sl_analysis", ret);
    ret = av_opt_set_int(dec, "export_sl_stages", 1, AV_OPT_SEARCH_CHILDREN);
    if (ret < 0) die("av_opt_set export_sl_stages", ret);

    ret = avcodec_open2(dec, codec, NULL);
    if (ret < 0) die("avcodec_open2", ret);

    pkt = av_packet_alloc();
    frm = av_frame_alloc();
    if (!pkt || !frm) { fprintf(stderr, "alloc failed\n"); exit(1); }

    FrameCapture *caps = calloc(MAX_FRAMES, sizeof(*caps));
    if (!caps) { fprintf(stderr, "OOM\n"); exit(1); }
    int nf = 0;

    while (1) {
        ret = av_read_frame(fmt, pkt);
        if (ret == AVERROR_EOF) {
            avcodec_send_packet(dec, NULL);
        } else if (ret < 0) {
            die("av_read_frame", ret);
        } else {
            if (pkt->stream_index != video_stream) {
                av_packet_unref(pkt);
                continue;
            }
            ret = avcodec_send_packet(dec, pkt);
            av_packet_unref(pkt);
            if (ret < 0) die("avcodec_send_packet", ret);
        }

        while (1) {
            ret = avcodec_receive_frame(dec, frm);
            if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) break;
            if (ret < 0) die("avcodec_receive_frame", ret);

            if (nf >= MAX_FRAMES) { fprintf(stderr, "Too many frames\n"); exit(1); }
            FrameCapture *fc = &caps[nf++];
            fc->frame_num = nf - 1;

            /* Pixel planes */
            int cw = (frm->width  + 1) / 2;
            int ch = (frm->height + 1) / 2;
            copy_plane(&fc->pixels[0], &fc->pixel_w[0], &fc->pixel_h[0],
                       frm->data[0], frm->linesize[0], frm->width, frm->height);
            copy_plane(&fc->pixels[1], &fc->pixel_w[1], &fc->pixel_h[1],
                       frm->data[1], frm->linesize[1], cw, ch);
            copy_plane(&fc->pixels[2], &fc->pixel_w[2], &fc->pixel_h[2],
                       frm->data[2], frm->linesize[2], cw, ch);

            copy_side_data(&fc->analysis, &fc->analysis_size,
                           AV_FRAME_DATA_SL_H264_ANALYSIS, frm);
            copy_side_data(&fc->stages,   &fc->stages_size,
                           AV_FRAME_DATA_SL_H264_STAGES,   frm);

            av_frame_unref(frm);
        }

        if (ret == AVERROR_EOF) break;
    }

    av_frame_free(&frm);
    av_packet_free(&pkt);
    avcodec_free_context(&dec);
    avformat_close_input(&fmt);

    *nframes_out = nf;
    return caps;
}

int main(void)
{
    printf("Decoding with threads=1 ...\n");
    int nf1;
    FrameCapture *caps1 = decode_all(1, &nf1);
    printf("  decoded %d frames\n", nf1);

    printf("Decoding with threads=4 (FF_THREAD_FRAME) ...\n");
    int nf4;
    FrameCapture *caps4 = decode_all(4, &nf4);
    printf("  decoded %d frames\n", nf4);

    if (nf1 != nf4) {
        fprintf(stderr, "MISMATCH: frame count threads=1(%d) vs threads=4(%d)\n", nf1, nf4);
        return 1;
    }

    int mismatches = 0;
    for (int f = 0; f < nf1; f++) {
        FrameCapture *a = &caps1[f];
        FrameCapture *b = &caps4[f];

        /* Pixel planes */
        for (int p = 0; p < 3; p++) {
            if (a->pixel_w[p] != b->pixel_w[p] || a->pixel_h[p] != b->pixel_h[p]) {
                fprintf(stderr, "frame %d plane %d: dimension mismatch %dx%d vs %dx%d\n",
                        f, p, a->pixel_w[p], a->pixel_h[p], b->pixel_w[p], b->pixel_h[p]);
                mismatches++;
                continue;
            }
            size_t sz = (size_t)a->pixel_w[p] * a->pixel_h[p];
            if (memcmp(a->pixels[p], b->pixels[p], sz) != 0) {
                fprintf(stderr, "frame %d plane %d: PIXEL MISMATCH\n", f, p);
                mismatches++;
            }
        }

        /* Analysis blob */
        if (a->analysis_size != b->analysis_size) {
            fprintf(stderr, "frame %d analysis: size mismatch %zu vs %zu\n",
                    f, a->analysis_size, b->analysis_size);
            mismatches++;
        } else if (a->analysis && b->analysis) {
            if (memcmp(a->analysis, b->analysis, a->analysis_size) != 0) {
                fprintf(stderr, "frame %d analysis: CONTENT MISMATCH\n", f);
                mismatches++;
            }
        } else if (a->analysis || b->analysis) {
            fprintf(stderr, "frame %d analysis: one side missing\n", f);
            mismatches++;
        }

        /* Stages blob */
        if (a->stages_size != b->stages_size) {
            fprintf(stderr, "frame %d stages: size mismatch %zu vs %zu\n",
                    f, a->stages_size, b->stages_size);
            mismatches++;
        } else if (a->stages && b->stages) {
            if (memcmp(a->stages, b->stages, a->stages_size) != 0) {
                fprintf(stderr, "frame %d stages: CONTENT MISMATCH\n", f);
                mismatches++;
            }
        } else if (a->stages || b->stages) {
            fprintf(stderr, "frame %d stages: one side missing\n", f);
            mismatches++;
        }
    }

    for (int f = 0; f < nf1; f++) free_frame_capture(&caps1[f]);
    for (int f = 0; f < nf4; f++) free_frame_capture(&caps4[f]);
    free(caps1);
    free(caps4);

    if (mismatches > 0) {
        fprintf(stderr, "\n%d mismatches — FRAME THREADING IS NOT BIT-EXACT\n", mismatches);
        return 1;
    }

    printf("\nAll %d frames bit-exact: pixels + analysis + stages. PASS\n", nf1);
    return 0;
}
