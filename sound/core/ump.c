// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * Universal MIDI Packet (UMP) support
 */

#include <linux/list.h>
#include <linux/slab.h>
#include <linux/module.h>
#include <linux/export.h>
#include <linux/mm.h>
#include <sound/core.h>
#include <sound/rawmidi.h>
#include <sound/ump.h>

#define ump_err(ump, fmt, args...)	dev_err(&(ump)->core.dev, fmt, ##args)
#define ump_warn(ump, fmt, args...)	dev_warn(&(ump)->core.dev, fmt, ##args)
#define ump_info(ump, fmt, args...)	dev_info(&(ump)->core.dev, fmt, ##args)
#define ump_dbg(ump, fmt, args...)	dev_dbg(&(ump)->core.dev, fmt, ##args)

static int snd_ump_dev_register(struct snd_rawmidi *rmidi);
static int snd_ump_dev_unregister(struct snd_rawmidi *rmidi);
static long snd_ump_ioctl(struct snd_rawmidi *rmidi, unsigned int cmd,
			  void __user *argp);
static void snd_ump_proc_read(struct snd_info_entry *entry,
			      struct snd_info_buffer *buffer);
static int snd_ump_rawmidi_open(struct snd_rawmidi_substream *substream);
static int snd_ump_rawmidi_close(struct snd_rawmidi_substream *substream);
static void snd_ump_rawmidi_trigger(struct snd_rawmidi_substream *substream,
				    int up);
static void snd_ump_rawmidi_drain(struct snd_rawmidi_substream *substream);
static void ump_handle_stream_msg(struct snd_ump_endpoint *ump);
static void snd_ump_watch_input(struct snd_ump_endpoint *ump,
				const unsigned char *buffer, int count);

static const struct snd_rawmidi_global_ops snd_ump_rawmidi_ops = {
	.dev_register = snd_ump_dev_register,
	.dev_unregister = snd_ump_dev_unregister,
	.ioctl = snd_ump_ioctl,
	.proc_read = snd_ump_proc_read,
};

static const struct snd_rawmidi_ops snd_ump_rawmidi_input_ops = {
	.open = snd_ump_rawmidi_open,
	.close = snd_ump_rawmidi_close,
	.trigger = snd_ump_rawmidi_trigger,
};

static const struct snd_rawmidi_ops snd_ump_rawmidi_output_ops = {
	.open = snd_ump_rawmidi_open,
	.close = snd_ump_rawmidi_close,
	.trigger = snd_ump_rawmidi_trigger,
	.drain = snd_ump_rawmidi_drain,
};

static void snd_ump_endpoint_free(struct snd_rawmidi *rmidi)
{
	struct snd_ump_endpoint *ump = rawmidi_to_ump(rmidi);
	struct snd_ump_block *fb;

	while (!list_empty(&ump->fb_list)) {
		fb = list_first_entry(&ump->fb_list, struct snd_ump_block, list);
		list_del(&fb->list);
		if (fb->private_free)
			fb->private_free(fb);
		kfree(fb);
	}

	if (ump->private_free)
		ump->private_free(ump);
}

/**
 * snd_ump_endpoint_new - create a UMP Endpoint object
 * @card: the card instance
 * @id: the id string for rawmidi
 * @device: the device index for rawmidi
 * @output: 1 for enabling output
 * @input: 1 for enabling input
 * @ump_ret: the pointer to store the new UMP instance
 *
 * Creates a new UMP Endpoint object. A UMP Endpoint is tied with one rawmidi
 * instance with one input and/or one output rawmidi stream (either uni-
 * or bi-directional). A UMP Endpoint may contain one or multiple UMP Blocks
 * that consist of one or multiple MIDI Endpoints.
 *
 * Use snd_rawmidi_set_ops() to set the operators to the new instance.
 * Unlike snd_rawmidi_new(), this sets up the info_flags by itself
 * depending on the given @output and @input.
 *
 * The device has SNDRV_RAWMIDI_INFO_UMP flag set and a different device
 * file ("umpCxDx") than a standard MIDI 1.x device ("midiCxDx") is
 * created.
 *
 * Return: Zero if successful, or a negative error code on failure.
 */
int snd_ump_endpoint_new(struct snd_card *card, char *id, int device,
			 int output, int input,
			 struct snd_ump_endpoint **ump_ret)
{
	unsigned int info_flags = SNDRV_RAWMIDI_INFO_UMP;
	struct snd_ump_endpoint *ump;
	int err;

	if (input)
		info_flags |= SNDRV_RAWMIDI_INFO_INPUT;
	if (output)
		info_flags |= SNDRV_RAWMIDI_INFO_OUTPUT;
	if (input && output)
		info_flags |= SNDRV_RAWMIDI_INFO_DUPLEX;

	ump = kzalloc(sizeof(*ump), GFP_KERNEL);
	if (!ump)
		return -ENOMEM;
	INIT_LIST_HEAD(&ump->fb_list);
	init_waitqueue_head(&ump->oob_wait);
	err = snd_rawmidi_init(&ump->core, card, id, device,
			       output, input, info_flags);
	if (err < 0) {
		snd_rawmidi_free(&ump->core);
		return err;
	}

	ump->info.card = card->number;
	ump->info.device = device;

	ump->core.private_free = snd_ump_endpoint_free;
	ump->core.ops = &snd_ump_rawmidi_ops;
	if (input)
		snd_rawmidi_set_ops(&ump->core, SNDRV_RAWMIDI_STREAM_INPUT,
				    &snd_ump_rawmidi_input_ops);
	if (output)
		snd_rawmidi_set_ops(&ump->core, SNDRV_RAWMIDI_STREAM_OUTPUT,
				    &snd_ump_rawmidi_output_ops);

	*ump_ret = ump;
	return 0;
}
EXPORT_SYMBOL_GPL(snd_ump_endpoint_new);

/*
 * Device register / unregister hooks
 */

#if IS_ENABLED(CONFIG_SND_SEQUENCER)
static void snd_ump_dev_seq_free(struct snd_seq_device *device)
{
	struct snd_ump_endpoint *ump = device->private_data;

	ump->seq_dev = NULL;
}
#endif

static int snd_ump_dev_register(struct snd_rawmidi *rmidi)
{
#if IS_ENABLED(CONFIG_SND_SEQUENCER)
	struct snd_ump_endpoint *ump = rawmidi_to_ump(rmidi);
	int err;

	err = snd_seq_device_new(ump->core.card, ump->core.device,
				 SNDRV_SEQ_DEV_ID_UMP, 0, &ump->seq_dev);
	if (err < 0)
		return err;
	ump->seq_dev->private_data = ump;
	ump->seq_dev->private_free = snd_ump_dev_seq_free;
	snd_device_register(ump->core.card, ump->seq_dev);
#endif
	return 0;
}

static int snd_ump_dev_unregister(struct snd_rawmidi *rmidi)
{
	return 0;
}

static struct snd_ump_block *
snd_ump_get_block(struct snd_ump_endpoint *ump, unsigned char id)
{
	struct snd_ump_block *fb;

	list_for_each_entry(fb, &ump->fb_list, list) {
		if (fb->info.block_id == id)
			return fb;
	}
	return NULL;
}

/*
 * rawmidi ops for UMP endpoint
 */
static int snd_ump_rawmidi_open(struct snd_rawmidi_substream *substream)
{
	struct snd_ump_endpoint *ump = rawmidi_to_ump(substream->rmidi);
	int dir = substream->stream;
	int err;

	if (ump->substreams[dir])
		return -EBUSY;
	err = ump->ops->open(ump, dir);
	if (err < 0)
		return err;
	ump->substreams[dir] = substream;
	return 0;
}

static int snd_ump_rawmidi_close(struct snd_rawmidi_substream *substream)
{
	struct snd_ump_endpoint *ump = rawmidi_to_ump(substream->rmidi);
	int dir = substream->stream;

	ump->substreams[dir] = NULL;
	ump->ops->close(ump, dir);
	return 0;
}

static void snd_ump_rawmidi_trigger(struct snd_rawmidi_substream *substream,
				    int up)
{
	struct snd_ump_endpoint *ump = rawmidi_to_ump(substream->rmidi);
	int dir = substream->stream;

	ump->ops->trigger(ump, dir, up);
}

static void snd_ump_rawmidi_drain(struct snd_rawmidi_substream *substream)
{
	struct snd_ump_endpoint *ump = rawmidi_to_ump(substream->rmidi);

	if (ump->ops->drain)
		ump->ops->drain(ump, SNDRV_RAWMIDI_STREAM_OUTPUT);
}

/**
 * snd_ump_receive - transfer UMP packets from the device
 * @ump: the UMP endpoint
 * @buffer: the buffer pointer to transfer
 * @count: byte size to transfer
 *
 * Called from the driver to submit the received UMP packets from the device
 * to user-space.  It's essentially a wrapper of rawmidi_receive().
 */
int snd_ump_receive(struct snd_ump_endpoint *ump,
		    const unsigned char *buffer, int count)
{
	struct snd_rawmidi_substream *substream =
		ump->substreams[SNDRV_RAWMIDI_STREAM_INPUT];

	snd_ump_watch_input(ump, buffer, count);
	if (!substream)
		return 0;
	return snd_rawmidi_receive(substream, buffer, count);
}
EXPORT_SYMBOL_GPL(snd_ump_receive);

/**
 * snd_ump_transmit - transmit UMP packets
 * @ump: the UMP endpoint
 * @buffer: the buffer pointer to transfer
 * @count: byte size to transfer
 *
 * Called from the driver to obtain the UMP packets from user-space to the
 * device.  It's essentially a wrapper of rawmidi_transmit().
 */
int snd_ump_transmit(struct snd_ump_endpoint *ump,
		     unsigned char *buffer, int count)
{
	struct snd_rawmidi_substream *substream =
		ump->substreams[SNDRV_RAWMIDI_STREAM_OUTPUT];

	if (!substream)
		return -ENODEV;
	return snd_rawmidi_transmit(substream, buffer, count);
}
EXPORT_SYMBOL_GPL(snd_ump_transmit);

/**
 * snd_ump_block_new - Create a UMP block
 * @ump: UMP object
 * @blk: block ID number to create
 * @direction: direction (in/out/bidirection)
 * @first_group: the first group ID
 * @num_groups: the number of groups in this block
 * @fb_ret: the point to store the resultant block object
 */
int snd_ump_block_new(struct snd_ump_endpoint *ump, unsigned int blk,
		      unsigned int direction, unsigned int first_group,
		      unsigned int num_groups, struct snd_ump_block **fb_ret)
{
	struct snd_ump_block *fb, *p;

	if (blk < 0 || blk >= SNDRV_UMP_MAX_BLOCKS)
		return -EINVAL;

	if (snd_ump_get_block(ump, blk))
		return -EBUSY;

	fb = kzalloc(sizeof(*fb), GFP_KERNEL);
	if (!fb)
		return -ENOMEM;

	fb->ump = ump;
	fb->info.card = ump->info.card;
	fb->info.device = ump->info.device;
	fb->info.block_id = blk;
	if (blk >= ump->info.num_blocks)
		ump->info.num_blocks = blk + 1;
	fb->info.direction = direction;
	fb->info.active = 1;
	fb->info.first_group = first_group;
	fb->info.num_groups = num_groups;

	/* put the entry in the ordered list */
	list_for_each_entry(p, &ump->fb_list, list) {
		if (p->info.block_id > blk) {
			list_add_tail(&fb->list, &p->list);
			goto added;
		}
	}
	list_add_tail(&fb->list, &ump->fb_list);

 added:
	*fb_ret = fb;
	return 0;
}
EXPORT_SYMBOL_GPL(snd_ump_block_new);

static int snd_ump_ioctl_block(struct snd_ump_endpoint *ump,
			       struct snd_ump_block_info __user *argp)
{
	struct snd_ump_block *fb;
	unsigned char id;

	if (get_user(id, &argp->block_id))
		return -EFAULT;
	fb = snd_ump_get_block(ump, id);
	if (!fb)
		return -ENOENT;
	if (copy_to_user(argp, &fb->info, sizeof(fb->info)))
		return -EFAULT;
	return 0;
}

static int seq_notify_protocol(struct snd_ump_endpoint *ump)
{
#if IS_ENABLED(CONFIG_SND_SEQUENCER)
	int err;

	if (ump->seq_ops && ump->seq_ops->switch_protocol) {
		err = ump->seq_ops->switch_protocol(ump);
		if (err < 0)
			return err;
	}
#endif /* CONFIG_SND_SEQUENCER */
	return 0;
}

static int try_to_switch_protocol(struct snd_ump_endpoint *ump,
				  unsigned int proto_req);

static int snd_ump_ioctl_switch_protocol(struct snd_ump_endpoint *ump,
					 unsigned int __user *arg)
{
	unsigned int proto;
	int err;

	if (get_user(proto, arg))
		return -EFAULT;
	if (ump->info.version) {
		err = try_to_switch_protocol(ump, proto);
		if (err)
			return err;
	} else if (ump->info.protocol != proto) {
		proto &= ump->info.protocol_caps;
		if (!proto ||
		    ((proto & SNDRV_UMP_EP_INFO_PROTO_MIDI1) &&
		     (proto & SNDRV_UMP_EP_INFO_PROTO_MIDI2)))
			return -EINVAL;
		ump->info.protocol = proto;
	}

	return seq_notify_protocol(ump);
}

/*
 * Handle UMP-specific ioctls; called from snd_rawmidi_ioctl()
 */
static long snd_ump_ioctl(struct snd_rawmidi *rmidi, unsigned int cmd,
			  void __user *argp)
{
	struct snd_ump_endpoint *ump = rawmidi_to_ump(rmidi);

	switch (cmd) {
	case SNDRV_UMP_IOCTL_ENDPOINT_INFO:
		if (copy_to_user(argp, &ump->info, sizeof(ump->info)))
			return -EFAULT;
		return 0;
	case SNDRV_UMP_IOCTL_BLOCK_INFO:
		return snd_ump_ioctl_block(ump, argp);
	case SNDRV_UMP_IOCTL_SWITCH_PROTOCOL:
		return snd_ump_ioctl_switch_protocol(ump, argp);
	default:
		dev_dbg(&rmidi->dev, "rawmidi: unknown command = 0x%x\n", cmd);
		return -ENOTTY;
	}
}

static const char *ump_direction_string(int dir)
{
	switch (dir) {
	case SNDRV_UMP_DIR_INPUT:
		return "input";
	case SNDRV_UMP_DIR_OUTPUT:
		return "output";
	case SNDRV_UMP_DIR_BIDIRECTION:
		return "bidirection";
	default:
		return "unknown";
	}
}

/* Additional proc file output */
static void snd_ump_proc_read(struct snd_info_entry *entry,
			      struct snd_info_buffer *buffer)
{
	struct snd_rawmidi *rmidi = entry->private_data;
	struct snd_ump_endpoint *ump = rawmidi_to_ump(rmidi);
	struct snd_ump_block *fb;

	snd_iprintf(buffer, "EP Name: %s\n", ump->info.name);
	snd_iprintf(buffer, "EP Product ID: %s\n", ump->info.product_id);
	snd_iprintf(buffer, "UMP Version: 0x%04x\n", ump->info.version);
	snd_iprintf(buffer, "Protocol Caps: 0x%08x\n", ump->info.protocol_caps);
	snd_iprintf(buffer, "Protocol: 0x%08x\n", ump->info.protocol);
	if (ump->info.version) {
		snd_iprintf(buffer, "Manufacturer ID: 0x%04x\n",
			    ump->info.manufacturer_id);
		snd_iprintf(buffer, "Family ID: 0x%04x\n", ump->info.family_id);
		snd_iprintf(buffer, "Model ID: 0x%04x\n", ump->info.model_id);
		snd_iprintf(buffer, "SW Revision: 0x%x\n", ump->info.sw_revision);
	}
	snd_iprintf(buffer, "Num Blocks: %d\n\n", ump->info.num_blocks);

	list_for_each_entry(fb, &ump->fb_list, list) {
		snd_iprintf(buffer, "Block %d (%s)\n", fb->info.block_id,
			    fb->info.name);
		snd_iprintf(buffer, "  Direction: %s\n",
			    ump_direction_string(fb->info.direction));
		snd_iprintf(buffer, "  Active: %s\n",
			    fb->info.active ? "Yes" : "No");
		snd_iprintf(buffer, "  Groups: %d-%d\n",
			    fb->info.first_group,
			    fb->info.first_group + fb->info.num_groups - 1);
		snd_iprintf(buffer, "  Is MIDI1: %s%s\n",
			    (fb->info.flags & SNDRV_UMP_BLOCK_IS_MIDI1) ? "Yes" : "No",
			    (fb->info.flags & SNDRV_UMP_BLOCK_IS_LOWSPEED) ? " (Low Speed)" : "");
		if (ump->info.version) {
			snd_iprintf(buffer, "  MIDI-CI Valid: %s\n",
				    fb->info.midi_ci_valid ? "Yes" : "No");
			if (fb->info.midi_ci_valid)
				snd_iprintf(buffer, "  MIDI-CI Version: %d\n",
					    fb->info.midi_ci_version);
			snd_iprintf(buffer, "  Sysex8 Streams: %d\n",
				    fb->info.sysex8_streams);
		}
		snd_iprintf(buffer, "\n");
	}
}

/*
 * UMP endpoint and function block handling
 */

/* number of 32bit words for each UMP message type */
static unsigned char ump_packet_words[0x10] = {
	1, 1, 1, 2, 2, 4, 1, 1, 2, 2, 2, 3, 3, 4, 4, 4
};

static bool snd_ump_parser_feed(struct snd_ump_parser_ctx *ctx, __le32 rawval)
{
	u32 val = le32_to_cpu(rawval);

	if (!ctx->remaining) {
		ctx->remaining = ump_packet_words[ump_message_type(val)];
		ctx->size = 0;
	}
	ctx->pack[ctx->size++] = val;
	return --ctx->remaining == 0;
}

/* open / close UMP streams for the internal out-of-bound communication */
static int ump_request_open(struct snd_ump_endpoint *ump)
{
	return snd_rawmidi_kernel_open(&ump->core, 0,
				       SNDRV_RAWMIDI_LFLG_OUTPUT,
				       &ump->oob_rfile);
}

static void ump_request_close(struct snd_ump_endpoint *ump)
{
	snd_rawmidi_kernel_release(&ump->oob_rfile);
}

/* try to setup via UMP stream messages */
static int __ump_req_msg(struct snd_ump_endpoint *ump, u32 req1, u32 req2)
{
	__le32 buf[4];

	memset(buf, 0, sizeof(buf));
	buf[0] = cpu_to_le32(req1);
	buf[1] = cpu_to_le32(req2);
	snd_rawmidi_kernel_write(ump->oob_rfile.output, (unsigned char *)&buf,
				 16);
	wait_event_timeout(ump->oob_wait, !ump->oob_response,
			   msecs_to_jiffies(500));
	if (ump->oob_response) {
		ump->oob_response = NULL;
		return -ETIMEDOUT;
	}
	return 0;
}

/* OOB-response callback for dealing with a single command */
static void ump_wait_for_cmd(struct snd_ump_endpoint *ump)
{
	const u32 *in_buf = ump->parser.pack;
	int size = ump->parser.size << 2;

	ump_dbg(ump, "%s: %08x vs wait-for %08x\n",
		__func__, in_buf[0], ump->oob_wait_for);
	if (ump->oob_wait_for != (in_buf[0] & 0xffff0000U))
		return;
	memcpy(ump->oob_buf_u32, in_buf, size);
	ump->oob_response = NULL;
	wake_up(&ump->oob_wait);
}

/* OOB-response callback for dealing with a string from UMP stream msg */
static void ump_wait_for_string(struct snd_ump_endpoint *ump)
{
	const u32 *in_buf = ump->parser.pack;
	int format, offset;

	ump_dbg(ump, "%s: %08x vs wait-for %08x\n",
		__func__, in_buf[0], ump->oob_wait_for);
	/* exclude the format bits */
	if (ump->oob_wait_for != (in_buf[0] & 0xf3ff0000U))
		return;
	if (ump->parser.size != 4)
		return;
	format = (in_buf[0] >> 26) & 3;
	if (ump_stream_message_status(in_buf[0]) == UMP_STREAM_MSG_STATUS_FB_NAME)
		offset = 3;
	else
		offset = 2;
	if (ump->oob_size + 16 <= ump->oob_maxsize) {
		for (; offset < 16; offset++)
			ump->oob_buf_string[ump->oob_size++] =
				in_buf[offset / 4] >> (3 - (offset % 4)) * 8;
	}

	if (format == 0 || format == 3) {
		ump->oob_response = NULL;
		wake_up(&ump->oob_wait);
	}
}

/* request a command and wait for the given response */
static int ump_req_msg(struct snd_ump_endpoint *ump, u32 req1, u32 req2,
		       u32 reply, u32 *buf)
{
	int ret;

	ump_dbg(ump, "%s: request %08x %08x, wait-for %08x\n",
		__func__, req1, req2, reply);
	memset(buf, 0, 16);
	ump->oob_wait_for = reply;
	ump->oob_response = ump_wait_for_cmd;
	ump->oob_buf_u32 = buf;
	ret = __ump_req_msg(ump, req1, req2);
	ump_dbg(ump, "%s: reply %d: %08x %08x %08x %08x\n",
		__func__, ret, buf[0], buf[1], buf[2], buf[3]);
	return ret;
}

/* request a command and wait for the string */
static int ump_req_str(struct snd_ump_endpoint *ump, u32 req1, u32 req2,
		       u32 reply, char *dest, int maxsize)
{
	int ret;

	ump_dbg(ump, "%s: request %08x %08x, wait-for %08x\n",
		__func__, req1, req2, reply);
	ump->oob_wait_for = reply;
	ump->oob_size = 0;
	ump->oob_buf_string = dest;
	ump->oob_maxsize = maxsize;
	ump->oob_response = ump_wait_for_string;
	ret = __ump_req_msg(ump, req1, req2);
	ump->oob_buf_string[ump->oob_size] = 0;
	ump_dbg(ump, "%s: reply %d: '%s'\n", __func__, ret, dest);
	return ret;
}

/* try to switch to the given protocol */
static int try_to_switch_protocol(struct snd_ump_endpoint *ump,
				  unsigned int proto_req)
{
	u32 buf[4];
	int err;

	ump_dbg(ump, "Try to switch protocol: %x - >%x\n",
		ump->info.protocol, proto_req);
	if (ump->info.protocol == proto_req)
		return 0;

	proto_req &= ump->info.protocol_caps;
	if (!proto_req) {
		ump_dbg(ump, "Protocol not supported\n");
		return -ENXIO;
	}

	err = ump_req_msg(ump,
			  ump_stream_compose(UMP_STREAM_MSG_STATUS_EP_PROTO_REQUEST, 0) |
			  proto_req, 0,
			  ump_stream_compose(UMP_STREAM_MSG_STATUS_EP_PROTO_NOTIFY, 0),
			  buf);
	if (err < 0) {
		ump_dbg(ump, "Failed to switch to protocol 0x%x\n", proto_req);
		return err;
	}

	ump->info.protocol = buf[0] & 0xffff;
	ump_info(ump, "Switched to protocol 0x%x\n", ump->info.protocol);
	return 0;
}

/* request a UMP EP command, receiving the reply at @buf (128 bits) */
static int
ump_request_ep_info(struct snd_ump_endpoint *ump, u32 req, u32 reply, u32 *buf)
{
	return ump_req_msg(ump,
			   ump_stream_compose(UMP_STREAM_MSG_STATUS_GET_EP, 0) |
			   0x0101, /* UMP version 1.1 */
			   req,
			   ump_stream_compose(reply, 0), buf);
}

/* request a UMP EP string command, receiving at @buf with @maxsize byte limit */
static int
ump_request_ep_string(struct snd_ump_endpoint *ump, u32 req, u32 reply,
		      char *buf, int maxsize)
{
	return ump_req_str(ump,
			   ump_stream_compose(UMP_STREAM_MSG_STATUS_GET_EP, 0) |
			   0x0101, /* UMP version 1.1 */
			   req,
			   ump_stream_compose(reply, 0), buf, maxsize);
}

/* request a FB info for the given block id @blk */
static int
ump_request_fb_info(struct snd_ump_endpoint *ump, unsigned char blk, u32 *buf)
{
	return ump_req_msg(ump,
			   ump_stream_compose(UMP_STREAM_MSG_STATUS_GET_FB, 0) |
			   (blk << 8) | UMP_STREAM_MSG_REQUEST_FB_INFO, 0,
			   ump_stream_compose(UMP_STREAM_MSG_STATUS_FB_INFO, 0),
			   buf);
}

/* request a FB name string for the given block @blk */
static int
ump_request_fb_name(struct snd_ump_endpoint *ump, unsigned char blk,
		    char *buf, int size)
{
	return ump_req_str(ump,
			   ump_stream_compose(UMP_STREAM_MSG_STATUS_GET_FB, 0) |
			   (blk << 8) | UMP_STREAM_MSG_REQUEST_FB_NAME, 0,
			   ump_stream_compose(UMP_STREAM_MSG_STATUS_FB_NAME, 0),
			   buf, size);
}

/* convert 3 byte manufacturer ID to 16bit */
static unsigned short get_16bit_man_id(u32 src)
{
	unsigned short val = (src >> 16) & 0x7f;

	if (!val)
		val = 0x8000 | (src & 0x7f7f);
	return val;
}

/* Extract Function Block info from UMP packet */
static void fill_fb_info(struct snd_ump_endpoint *ump,
			 struct snd_ump_block_info *info,
			 const u32 *buf)
{
	info->direction = buf[0] & 3;
	info->first_group = buf[1] >> 24;
	info->num_groups = (buf[1] >> 16) & 0xff;
	info->flags = (buf[0] >> 2) & 3;
	info->active = (buf[0] >> 15) & 1;
	info->midi_ci_valid = (buf[1] >> 15) & 1;
	info->midi_ci_version = (buf[1] >> 8) & 0x7f;
	info->sysex8_streams = buf[1] & 0xff;

	ump_dbg(ump, "FB %d: dir=%d, active=%d, first_gp=%d, num_gp=%d, midici=%d:%d, sysex8=%d, flags=0x%x\n",
		info->block_id, info->direction, info->active,
		info->first_group, info->num_groups,
		info->midi_ci_valid, info->midi_ci_version,
		info->sysex8_streams, info->flags);
}

/**
 * snd_ump_parse_endpoint - parse endpoint and create function blocks
 * @ump: UMP object
 */
int snd_ump_parse_endpoint(struct snd_ump_endpoint *ump)
{
	struct snd_ump_block *fb;
	u32 buf[4];
	int blk, err;

	if (!(ump->core.info_flags & SNDRV_RAWMIDI_INFO_DUPLEX))
		return -ENXIO;

	err = ump_request_open(ump);
	if (err < 0) {
		ump_dbg(ump, "Unable to open rawmidi device: %d\n", err);
		return err;
	}

	/* Check Endpoint Information */
	err = ump_request_ep_info(ump, UMP_STREAM_MSG_REQUEST_EP_INFO,
				  UMP_STREAM_MSG_STATUS_EP_INFO, buf);
	if (err < 0) {
		ump_dbg(ump, "Unable to get UMP EP info\n");
		goto error;
	}

	ump->info.version = buf[0] & 0xffff;
	ump->info.num_blocks = buf[1] >> 24;
	if (ump->info.num_blocks > 0x20) {
		ump_info(ump, "Invalid function blocks %d, fallback to 1\n",
			 ump->info.num_blocks);
		ump->info.num_blocks = 1;
	}

	ump->info.protocol_caps = buf[1] & 0xffff;

	ump_dbg(ump, "Got EP info: version=%x, num_blocks=%x, proto_caps=%x\n",
		ump->info.version, ump->info.num_blocks, ump->info.protocol_caps);

	/* Request Endpoint Device Info */
	err = ump_request_ep_info(ump, UMP_STREAM_MSG_REQUEST_EP_DEVICE,
				  UMP_STREAM_MSG_STATUS_EP_DEVICE, buf);
	if (err < 0) {
		ump_dbg(ump, "Unable to get UMP EP device info\n");
	} else {
		ump->info.manufacturer_id = get_16bit_man_id(buf[1]);
		ump->info.family_id = (buf[2] >> 16) & 0x7f7f;
		ump->info.model_id = buf[2] & 0x7f7f;
		ump->info.sw_revision = buf[3];
		ump_dbg(ump, "Go EP devinfo: manid=%x, family=%x, model=%x, sw=%x\n",
			ump->info.manufacturer_id,
			ump->info.family_id,
			ump->info.model_id,
			ump->info.sw_revision);
	}

	/* Request Endpoint Name */
	err = ump_request_ep_string(ump, UMP_STREAM_MSG_REQUEST_EP_NAME,
				    UMP_STREAM_MSG_STATUS_EP_NAME,
				    ump->info.name, sizeof(ump->info.name) - 1);
	if (err < 0)
		ump_dbg(ump, "Unable to get UMP EP name string\n");

	/* Request Endpoint Product ID */
	err = ump_request_ep_string(ump, UMP_STREAM_MSG_REQUEST_EP_PID,
				    UMP_STREAM_MSG_STATUS_EP_PID,
				    ump->info.product_id,
				    sizeof(ump->info.product_id) - 1);
	if (err < 0)
		ump_dbg(ump, "Unable to get UMP EP product ID string\n");

	/* Get the current protocol */
	err = ump_request_ep_info(ump, UMP_STREAM_MSG_REQUEST_EP_PROTO,
				  UMP_STREAM_MSG_STATUS_EP_PROTO_NOTIFY, buf);
	if (err < 0)
		ump_dbg(ump, "Unable to get UMP EP protocol info\n");
	else
		ump->info.protocol = buf[0] & 0xffff;

	/* Try to switch to MIDI 2.0 protocol (if available) */
	if (try_to_switch_protocol(ump, ump->info.protocol_caps & ~SNDRV_UMP_EP_INFO_PROTO_MIDI1) < 0)
		try_to_switch_protocol(ump, ump->info.protocol_caps & ~SNDRV_UMP_EP_INFO_PROTO_MIDI2);

	/* Get Function Block information */
	for (blk = 0; blk < ump->info.num_blocks; blk++) {
		err = ump_request_fb_info(ump, blk, buf);
		if (err < 0) {
			ump_dbg(ump, "Unable to get FB info for block %d\n", blk);
			break;
		}
		err = snd_ump_block_new(ump, blk,
					buf[0] & 3, /* direction */
					buf[1] >> 24, /* first group */
					(buf[1] >> 16) & 0xff, /* num roups */
					&fb);
		if (err < 0)
			goto error;

		fill_fb_info(ump, &fb->info, buf);

		err = ump_request_fb_name(ump, blk, fb->info.name,
					  sizeof(fb->info.name) - 1);
		if (err)
			ump_dbg(ump, "Unable to get UMP FB name string #%d\n", blk);
	}

	/* start watching FB info changes */
	ump->oob_response = ump_handle_stream_msg;

 error:
	ump_request_close(ump);
	return 0;
}
EXPORT_SYMBOL_GPL(snd_ump_parse_endpoint);

/* OOB handling of dynamic FB info update */
static void ump_handle_stream_msg(struct snd_ump_endpoint *ump)
{
	struct snd_ump_block *fb;
	struct snd_ump_block_info *info;
	const u32 *buf = ump->parser.pack;
	unsigned int blk;
	char tmpbuf[offsetof(struct snd_ump_block_info, name)];

	if (ump_message_type(*buf) != UMP_MSG_TYPE_UMP_STREAM ||
	    ump_stream_message_status(*buf) != UMP_STREAM_MSG_STATUS_FB_INFO)
		return;

	blk = (buf[0] >> 8) & 0x1f;
	fb = snd_ump_get_block(ump, blk);
	if (!fb) {
		/* FIXME: create new? */
		ump_info(ump, "Function Block Info Update for non-existing block %d\n",
			 blk);
		return;
	}

	/* check the FB info update */
	info = (struct snd_ump_block_info *)tmpbuf;
	fill_fb_info(ump, info, buf);
	if (!memcmp(&fb->info, tmpbuf, sizeof(tmpbuf)))
		return; /* no content change */

	/* update the actual FB info */
	memcpy(&fb->info, tmpbuf, sizeof(tmpbuf));

	/* unlike other OOB handling, this keeps oob_response */
}

/* Snoop UMP messages and process internally for OOB handling */
static void snd_ump_watch_input(struct snd_ump_endpoint *ump,
				const unsigned char *buffer, int count)
{
	const __le32 *buf;

	if (!count || !buffer)
		return;
	count >>= 2;
	buf = (const __le32 *)buffer;
	for (; count > 0; count--, buf++) {
		if (snd_ump_parser_feed(&ump->parser, *buf))
			if (ump->oob_response)
				ump->oob_response(ump);
	}
}

MODULE_DESCRIPTION("Universal MIDI Packet (UMP) Core Driver");
MODULE_LICENSE("GPL");
