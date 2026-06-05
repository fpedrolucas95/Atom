use crate::commands::{CommandContext, CommandResult};
use crate::parser::ParsedCommand;
use alloc::format;
use libipc::protocol::lookup_service;
use libnet::net_get_config;

pub fn cmd_ifconfig(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let netd_port = match lookup_service("netd") {
        Ok(port) => port,
        Err(_) => {
            ctx.error("ifconfig: network daemon (netd) not found");
            return CommandResult::Error;
        }
    };

    match net_get_config(netd_port) {
        Ok(cfg) => {
            ctx.println("");
            ctx.println("eth0: flags=UP,BROADCAST,RUNNING");
            ctx.println(&format!("        inet {}  netmask {}", cfg.ip, cfg.netmask));
            ctx.println(&format!("        gateway {}", cfg.gateway));
            ctx.println(&format!("        dns {}", cfg.dns));
            ctx.println(&format!(
                "        ether {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                cfg.mac[0], cfg.mac[1], cfg.mac[2], cfg.mac[3], cfg.mac[4], cfg.mac[5]
            ));
            ctx.println("");
            CommandResult::Ok
        }
        Err(e) => {
            ctx.error(&format!("ifconfig: failed to get config: {:?}", e));
            CommandResult::Error
        }
    }
}
