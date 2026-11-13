#!/bin/sh
# Phase 6 on the phone: how fast agentos-llm reads a prompt and writes an answer, with each of its
# ARM kernels, so the gain of each is measured on the device itself:
#   i8mm      SMMLA, two rows x two tokens per instruction (ARMv8.6+, e.g. Cortex-A510/A710)
#   dotprod   SDOT, weights unpacked once per block for every token (ARMv8.2+, e.g. A55/A75)
#   portable  plain Rust (e.g. Cortex-A53, PinePhone)
# A CPU without a kernel just repeats the slower one. Stop agentd's own model first if RAM is short.
#   sh phone-bench.sh /var/lib/agentd/models/qwen2.5-0.5b-instruct-q4_k_m.gguf [threads]
# POSIX sh (busybox ash).
MODEL=${1:?usage: phone-bench.sh MODEL.gguf [threads]}
THREADS=${2:-$(nproc)}
PORT=18890
LLM=${LLM:-agentos-llm}
here=$(cd "$(dirname "$0")" && pwd)
SYS="You are the agent that runs this phone. There is no screen: every request is done by calling exactly one tool. Answer with one JSON object and nothing else."
# a long prompt, as agentd's system prompt is (~700 tokens): the same text a few times
LONG=""
for i in 1 2 3 4 5 6 7 8; do LONG="$LONG $SYS Tools: call(to), send_sms(to, text), read_messages(), set_volume(percent), flashlight(on), wifi_scan(), where_am_i()."; done

echo "model: $MODEL"
echo "cpu: $(grep -m1 -i 'model name\|cpu part' /proc/cpuinfo | sed 's/.*: //'), $(nproc) cores, using $THREADS"
echo "features: $(grep -m1 Features /proc/cpuinfo | grep -o 'asimddp\|i8mm' | tr '\n' ' ')"
printf '%-10s %14s %14s\n' kernel "prompt tok/s" "answer tok/s"
for k in i8mm dotprod portable; do
	case $k in
	i8mm) env="" ;;
	dotprod) env="AGENTOS_NO_I8MM=1" ;;
	portable) env="AGENTOS_NO_I8MM=1 AGENTOS_NO_DOTPROD=1" ;;
	esac
	env $env $LLM -m "$MODEL" --port $PORT -c 2048 -t "$THREADS" >/dev/null 2>&1 &
	pid=$!
	i=0
	until wget -q -O /dev/null "http://127.0.0.1:$PORT/health" 2>/dev/null || [ $i -gt 120 ]; do sleep 1; i=$((i + 1)); done
	body="{\"prompt\":\"<|im_start|>system\\n$LONG<|im_end|>\\n<|im_start|>user\\nring ravi<|im_end|>\\n<|im_start|>assistant\\n\",\"n_predict\":32,\"temperature\":0}"
	r=$(wget -q -O - --post-data="$body" --header='Content-Type: application/json' "http://127.0.0.1:$PORT/completion" 2>/dev/null)
	pps=$(echo "$r" | sed -n 's/.*"prompt_per_second":\([0-9.]*\).*/\1/p')
	aps=$(echo "$r" | sed -n 's/.*"predicted_per_second":\([0-9.]*\).*/\1/p')
	pn=$(echo "$r" | sed -n 's/.*"prompt_n":\([0-9]*\).*/\1/p')
	printf '%-10s %14.1f %14.1f   (%s prompt tokens)\n' $k "${pps:-0}" "${aps:-0}" "${pn:-?}"
	kill $pid 2>/dev/null
	wait $pid 2>/dev/null
done
echo "paste these lines back to Claude"
