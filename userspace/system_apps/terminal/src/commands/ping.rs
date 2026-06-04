use crate::commands::{CommandContext, CommandResult};
use crate::parser::ParsedCommand;
use libnet::{net_resolve, net_icmp_echo, NetError};
use libipc::protocol::lookup_service;
use alloc::format;
use alloc::vec::Vec;

pub fn cmd_ping(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    if cmd.arg_count == 0 {
        ctx.error("Usage: ping <host> [-c count] [-i interval_ms] [-t timeout_ms]");
        return CommandResult::Error;
    }

    let host = cmd.args[0];
    let mut count = 4;
    let mut interval_ms = 1000;
    let mut timeout_ms = 2000;

    // Simple arg parsing
    let mut i = 1;
    while i < cmd.arg_count {
        match cmd.args[i] {
            "-c" if i + 1 < cmd.args.len() => {
                if let Ok(val) = cmd.args[i + 1].parse::<u32>() {
                    count = val;
                    i += 2;
                } else { i += 1; }
            }
            "-i" if i + 1 < cmd.args.len() => {
                if let Ok(val) = cmd.args[i + 1].parse::<u32>() {
                    interval_ms = val;
                    i += 2;
                } else { i += 1; }
            }
            "-t" if i + 1 < cmd.args.len() => {
                if let Ok(val) = cmd.args[i + 1].parse::<u32>() {
                    timeout_ms = val;
                    i += 2;
                } else { i += 1; }
            }
            _ => { i += 1; }
        }
    }

    // 1. Lookup netd
    let netd_port = match lookup_service("netd") {
        Ok(port) => port,
        Err(_) => {
            ctx.error("ping: network daemon (netd) not found");
            return CommandResult::Error;
        }
    };

    // 2. Resolve host
    ctx.println(&format!("PING {}...", host));
    let dest_ip = match net_resolve(netd_port, host) {
        Ok(ip) => ip,
        Err(_) => {
            ctx.error(&format!("ping: unknown host {}", host));
            return CommandResult::Error;
        }
    };

    ctx.println(&format!("PING {} ({}): 56 data bytes", host, dest_ip));

    let mut transmitted = 0;
    let mut received = 0;
    let mut rtts = Vec::new();

    for seq in 1..=count {
        transmitted += 1;
        let payload = [0u8; 56]; // Standard ping payload size

        match net_icmp_echo(netd_port, dest_ip, seq as u16, &payload, timeout_ms) {
            Ok(reply) => {
                received += 1;
                rtts.push(reply.rtt_ms);
                ctx.println(&format!(
                    "64 bytes from {}: icmp_seq={} ttl={} time={}.0 ms",
                    reply.src_ip, reply.sequence, reply.ttl, reply.rtt_ms
                ));
            }
            Err(NetError::Timeout) => {
                ctx.println(&format!("Request timeout for icmp_seq {}", seq));
            }
            Err(e) => {
                ctx.error(&format!("ping: error {:?}", e));
                break;
            }
        }

        if seq < count {
            atom_syscall::thread::sleep_ms(interval_ms as u64);
        }
    }

    // Statistics
    ctx.println(&format!("\n--- {} ping statistics ---", host));
    let loss = if transmitted > 0 {
        (transmitted - received) * 100 / transmitted
    } else {
        0
    };
    ctx.println(&format!(
        "{} packets transmitted, {} received, {}% packet loss",
        transmitted, received, loss
    ));

    if !rtts.is_empty() {
        let min = rtts.iter().min().unwrap();
        let max = rtts.iter().max().unwrap();
        let sum: u32 = rtts.iter().sum();
        let avg = sum / rtts.len() as u32;
        ctx.println(&format!(
            "rtt min/avg/max = {}.0/{}.0/{}.0 ms",
            min, avg, max
        ));
    }

    CommandResult::Ok
}
