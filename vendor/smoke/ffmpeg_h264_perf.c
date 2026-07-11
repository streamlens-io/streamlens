/*
 * Streamlens perf tool: measure ms/frame decoding a video file.
 *
 * Usage: ffmpeg_h264_perf <file> <threads>  [analysis=0|1]
 *   threads: 0=auto, 1=single, N=N frame threads
 *   analysis: 0=off (default), 1=enable export_sl_analysis (stages=off)
 *
 * Decodes all frames, measures wall time, prints ms/frame.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/frame.h>
#include <libavutil/opt.h>

static void die(const char *msg, int ret)
{
    char errbuf[128];
    av_strerror(ret, errbuf, sizeof(errbuf));
    fprintf(stderr, "FATAL: %s: %s\n", msg, errbuf);
    exit(1);
}

static double now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

int main(int argc, char **argv)
{
    if (argc < 3) {
        fprintf(stderr, "Usage: %s <file> <threads> [analysis=0|1]\n", argv[0]);
        return 1;
    }
    const char *path      = argv[1];
    int nthreads          = atoi(argv[2]);
    int analysis          = (argc >= 4) ? atoi(argv[3]) : 0;

    AVFormatContext *fmt = NULL;
    AVCodecContext  *dec = NULL;
    AVPacket        *pkt = NULL;
    AVFrame         *frm = NULL;
    const AVCodec   *codec;
    int video_stream = -1;
    int ret;

    ret = avformat_open_input(&fmt, path, NULL, NULL);
    if (ret < 0) die("avformat_open_input", ret);
    ret = avformat_find_stream_info(fmt, NULL);
    if (ret < 0) die("avformat_find_stream_info", ret);

    for (unsigned i = 0; i < fmt->nb_streams; i++) {
        if (fmt->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_VIDEO) {
            video_stream = (int)i;
            break;
        }
    }
    if (video_stream < 0) { fprintf(stderr, "No video stream\n"); return 1; }

    codec = avcodec_find_decoder(fmt->streams[video_stream]->codecpar->codec_id);
    if (!codec) { fprintf(stderr, "No decoder\n"); return 1; }

    dec = avcodec_alloc_context3(codec);
    if (!dec) { fprintf(stderr, "avcodec_alloc_context3 failed\n"); return 1; }

    ret = avcodec_parameters_to_context(dec, fmt->streams[video_stream]->codecpar);
    if (ret < 0) die("avcodec_parameters_to_context", ret);

    dec->thread_count = nthreads;
    dec->thread_type  = FF_THREAD_FRAME;

    if (analysis) {
        ret = av_opt_set_int(dec, "export_sl_analysis", 1, AV_OPT_SEARCH_CHILDREN);
        if (ret < 0) die("av_opt_set export_sl_analysis", ret);
    }

    ret = avcodec_open2(dec, codec, NULL);
    if (ret < 0) die("avcodec_open2", ret);

    pkt = av_packet_alloc();
    frm = av_frame_alloc();
    if (!pkt || !frm) { fprintf(stderr, "alloc failed\n"); return 1; }

    int nframes = 0;
    double t_start = now_ms();

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
            av_frame_unref(frm);
            nframes++;
        }

        if (ret == AVERROR_EOF) break;
    }

    double elapsed = now_ms() - t_start;
    double ms_per_frame = nframes > 0 ? elapsed / nframes : 0;

    printf("File:     %s\n", path);
    printf("Threads:  %d (0=auto)  type=FF_THREAD_FRAME\n", nthreads);
    printf("Analysis: %s\n", analysis ? "on" : "off");
    printf("Frames:   %d\n", nframes);
    printf("Total:    %.1f ms\n", elapsed);
    printf("ms/frame: %.2f\n", ms_per_frame);

    av_frame_free(&frm);
    av_packet_free(&pkt);
    avcodec_free_context(&dec);
    avformat_close_input(&fmt);
    return 0;
}
