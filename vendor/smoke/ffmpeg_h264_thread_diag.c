/*
 * Diagnostic: compare threads=1 vs threads=4 analysis for frame 1.
 * Prints the first few MB records that differ.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/frame.h>
#include <libavutil/opt.h>
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

typedef struct { uint8_t *data; size_t size; } Blob;

static Blob *decode_analyses(int nthreads, int *nframes_out)
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
            video_stream = (int)i; break;
        }
    }
    codec = avcodec_find_decoder(fmt->streams[video_stream]->codecpar->codec_id);
    dec = avcodec_alloc_context3(codec);
    avcodec_parameters_to_context(dec, fmt->streams[video_stream]->codecpar);
    dec->thread_count = nthreads;
    dec->thread_type  = FF_THREAD_FRAME;
    av_opt_set_int(dec, "export_sl_analysis", 1, AV_OPT_SEARCH_CHILDREN);
    avcodec_open2(dec, codec, NULL);

    pkt = av_packet_alloc();
    frm = av_frame_alloc();

    Blob *blobs = calloc(MAX_FRAMES, sizeof(*blobs));
    int nf = 0;

    while (1) {
        ret = av_read_frame(fmt, pkt);
        if (ret == AVERROR_EOF) {
            avcodec_send_packet(dec, NULL);
        } else if (ret < 0) {
            die("av_read_frame", ret);
        } else {
            if (pkt->stream_index != video_stream) { av_packet_unref(pkt); continue; }
            avcodec_send_packet(dec, pkt);
            av_packet_unref(pkt);
        }

        while (1) {
            ret = avcodec_receive_frame(dec, frm);
            if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF) break;
            if (ret < 0) die("avcodec_receive_frame", ret);

            const AVFrameSideData *sd = av_frame_get_side_data(frm, AV_FRAME_DATA_SL_H264_ANALYSIS);
            if (sd) {
                blobs[nf].data = malloc(sd->size);
                memcpy(blobs[nf].data, sd->data, sd->size);
                blobs[nf].size = sd->size;
            }
            nf++;
            av_frame_unref(frm);
        }
        if (ret == AVERROR_EOF) break;
    }

    av_frame_free(&frm);
    av_packet_free(&pkt);
    avcodec_free_context(&dec);
    avformat_close_input(&fmt);
    *nframes_out = nf;
    return blobs;
}

int main(void)
{
    int nf1, nf4;
    Blob *b1 = decode_analyses(1, &nf1);
    Blob *b4 = decode_analyses(4, &nf4);

    printf("frames: t1=%d t4=%d\n", nf1, nf4);

    /* Inspect frame 1 (first non-IDR) differences */
    for (int fr = 1; fr <= 5 && fr < nf1; fr++) {
        Blob *a = &b1[fr];
        Blob *b = &b4[fr];
        if (!a->data || !b->data || a->size != b->size) {
            printf("frame %d: size %zu vs %zu\n", fr, a->size, b->size);
            continue;
        }
        if (memcmp(a->data, b->data, a->size) == 0) { printf("frame %d: MATCH\n", fr); continue; }

        size_t hdr = sizeof(SLH264AnalysisHeader);
        SLH264AnalysisHeader *h1 = (void*)a->data;
        SLH264MBRecord *r1 = (void*)(a->data + hdr);
        SLH264MBRecord *r4 = (void*)(b->data + hdr);
        unsigned nb = h1->mb_width * h1->mb_height;
        printf("frame %d: %ux%u MBs, %u total\n", fr, h1->mb_width, h1->mb_height, nb);
        int shown = 0;
        for (unsigned i = 0; i < nb && shown < 10; i++) {
            if (memcmp(&r1[i], &r4[i], sizeof(SLH264MBRecord)) != 0) {
                printf("  MB %u: mb_type t1=%08x t4=%08x  bits t1=%u t4=%u  "
                       "qp t1=%d t4=%d  slice_id t1=%u t4=%u\n",
                       i, r1[i].mb_type, r4[i].mb_type,
                       r1[i].bits, r4[i].bits,
                       r1[i].qp, r4[i].qp,
                       r1[i].slice_id, r4[i].slice_id);
                shown++;
            }
        }
    }
    return 0;
}
