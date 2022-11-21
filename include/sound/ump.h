/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * Universal MIDI Packet (UMP) Support
 */
#ifndef __SOUND_UMP_H
#define __SOUND_UMP_H

#include <sound/rawmidi.h>

struct snd_ump_endpoint;
struct snd_ump_block;
struct snd_ump_ops;

struct snd_ump_endpoint {
	struct snd_rawmidi core;	/* raw UMP access */

	struct snd_ump_endpoint_info info;

	const struct snd_ump_ops *ops;	/* UMP ops set by the driver */
	struct snd_rawmidi_substream *substreams[2];	/* opened substreams */

	void *private_data;
	void (*private_free)(struct snd_ump_endpoint *ump);

	/* out-of-bound command processing */
	u32 oob_wait_for;
	union {
		u32 *oob_buf_u32;
		char *oob_buf_string;
	};
	int oob_size, oob_maxsize;
	wait_queue_head_t oob_wait;
	int (*oob_response)(struct snd_ump_endpoint *ump, const u32 *pack, int size);
	struct snd_rawmidi_file oob_rfile;

	struct list_head fb_list;	/* list of snd_ump_block objects */

#if IS_ENABLED(CONFIG_SND_SEQUENCER)
	struct snd_seq_device *seq_dev;
	const struct snd_seq_ump_ops *seq_ops;
	void *seq_client;
#endif
};

/* ops filled by UMP drivers */
struct snd_ump_ops {
	int (*open)(struct snd_ump_endpoint *ump, int dir);
	void (*close)(struct snd_ump_endpoint *ump, int dir);
	void (*trigger)(struct snd_ump_endpoint *ump, int dir, int up);
	void (*drain)(struct snd_ump_endpoint *ump, int dir);
};

/* ops filled by sequencer binding */
struct snd_seq_ump_ops {
	int (*switch_protocol)(struct snd_ump_endpoint *ump);
};

struct snd_ump_block {
	struct snd_ump_block_info info;
	struct snd_ump_endpoint *ump;

	void *private_data;
	void (*private_free)(struct snd_ump_block *fb);

	struct list_head list;
};

#define rawmidi_to_ump(rmidi)	container_of(rmidi, struct snd_ump_endpoint, core)

int snd_ump_endpoint_new(struct snd_card *card, char *id, int device,
			 int output, int input,
			 struct snd_ump_endpoint **ump_ret);
int snd_ump_parse_endpoint(struct snd_ump_endpoint *ump);
int snd_ump_block_new(struct snd_ump_endpoint *ump, unsigned int blk,
		      unsigned int direction, unsigned int first_group,
		      unsigned int num_groups, struct snd_ump_block **fb_ret);
int snd_ump_receive(struct snd_ump_endpoint *ump,
		    const unsigned char *buffer, int count);
int snd_ump_transmit(struct snd_ump_endpoint *ump,
		     unsigned char *buffer, int count);

/*
 * Some definitions for UMP
 */

/* MIDI 2.0 Message Type */
enum {
	UMP_MSG_TYPE_UTILITY	= 0x00,
	UMP_MSG_TYPE_SYSTEM	= 0x01,
	UMP_MSG_TYPE_MIDI1	= 0x02,
	UMP_MSG_TYPE_SYSEX7	= 0x03,
	UMP_MSG_TYPE_MIDI2	= 0x04,
	UMP_MSG_TYPE_DATA8	= 0x05,
	UMP_MSG_TYPE_FLEX_DATA	= 0x0d,
	UMP_MSG_TYPE_UMP_STREAM	= 0x0f,
};

/* MIDI 2.0 SysEx / Data Running Status */
enum {
	UMP_SYSEX_STATUS_SINGLE		= 0,
	UMP_SYSEX_STATUS_START		= 1,
	UMP_SYSEX_STATUS_CONTINUE	= 2,
	UMP_SYSEX_STATUS_END		= 3,
};

/* UMP Stream Message Status (type 0xf) */
enum {
	UMP_STREAM_MSG_STATUS_GET_EP		= 0x00,
	UMP_STREAM_MSG_STATUS_EP_INFO		= 0x01,
	UMP_STREAM_MSG_STATUS_EP_DEVICE		= 0x02,
	UMP_STREAM_MSG_STATUS_EP_NAME		= 0x03,
	UMP_STREAM_MSG_STATUS_EP_PID		= 0x04,
	UMP_STREAM_MSG_STATUS_EP_PROTO_REQUEST	= 0x05,
	UMP_STREAM_MSG_STATUS_EP_PROTO_NOTIFY	= 0x06,
	UMP_STREAM_MSG_STATUS_GET_FB		= 0x10,
	UMP_STREAM_MSG_STATUS_FB_INFO		= 0x11,
	UMP_STREAM_MSG_STATUS_FB_NAME		= 0x12,
};

/* UMP Get Endpoint filter bitmap */
enum {
	UMP_STREAM_MSG_REQUEST_EP_INFO		= (1U << 0),
	UMP_STREAM_MSG_REQUEST_EP_DEVICE	= (1U << 1),
	UMP_STREAM_MSG_REQUEST_EP_NAME		= (1U << 2),
	UMP_STREAM_MSG_REQUEST_EP_PID		= (1U << 3),
	UMP_STREAM_MSG_REQUEST_EP_PROTO		= (1U << 4),
};

/* UMP Get function block filter bitmap */
enum {
	UMP_STREAM_MSG_REQUEST_FB_INFO		= (1U << 0),
	UMP_STREAM_MSG_REQUEST_FB_NAME		= (1U << 1),
};

/* UMP Endpoint Info capability bits (used for protocol request/notify, too) */
enum {
	UMP_STREAM_MSG_EP_INFO_CAP_TXJR		= (1U << 0), /* Sending JRTS */
	UMP_STREAM_MSG_EP_INFO_CAP_RXJR		= (1U << 1), /* Receiving JRTS */
	UMP_STREAM_MSG_EP_INFO_CAP_MIDI1	= (1U << 8), /* MIDI 1.0 */
	UMP_STREAM_MSG_EP_INFO_CAP_MIDI2	= (1U << 9), /* MIDI 2.0 */
};

/* UMP Utility Type Status (type 0x0) */
enum {
	UMP_UTILITY_MSG_STATUS_NOOP		= 0x00,
	UMP_UTILITY_MSG_STATUS_JR_CLOCK		= 0x01,
	UMP_UTILITY_MSG_STATUS_JR_TSTAMP	= 0x02,
	UMP_UTILITY_MSG_STATUS_DCTPQ		= 0x03,
	UMP_UTILITY_MSG_STATUS_DC		= 0x04,
	UMP_UTILITY_MSG_STATUS_START_CLIP	= 0x05,
	UMP_UTILITY_MSG_STATUS_END_CLIP		= 0x06,
};

/*
 * Helpers for retrieving / filling bits from UMP
 */
static inline unsigned char ump_message_type(u32 data)
{
	return data >> 28;
}

static inline unsigned char ump_message_group(u32 data)
{
	return (data >> 24) & 0x0f;
}

static inline unsigned char ump_message_status_code(u32 data)
{
	return (data >> 16) & 0xf0;
}

static inline unsigned char ump_message_channel(u32 data)
{
	return (data >> 16) & 0x0f;
}

static inline u32 ump_compose(unsigned char type, unsigned char group,
			      unsigned char status)
{
	return ((u32)type << 28) | ((u32)group << 24) | ((u32)status << 16);
}

static inline unsigned char ump_sysex_message_status(u32 data)
{
	return (data >> 20) & 0xf;
}

static inline unsigned char ump_sysex_message_length(u32 data)
{
	return (data >> 16) & 0xf;
}

/* For Stream Messages */
static inline unsigned char ump_stream_message_format(u32 data)
{
	return (data >> 26) & 0x03;
}

static inline unsigned char ump_stream_message_status(u32 data)
{
	return (data >> 16) & 0x3ff;
}

static inline u32 ump_stream_compose(unsigned char status, unsigned short form)
{
	return (UMP_MSG_TYPE_UMP_STREAM << 28) | ((u32)form << 26) |
		((u32)status << 16);
}

#endif /* __SOUND_UMP_H */
