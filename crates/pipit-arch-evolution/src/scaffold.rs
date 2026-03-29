//! Architecture-to-Code Scaffold Generator — Task 10.3
//!
//! Maps genome → buildable project: directory structure, service boilerplate,
//! communication wiring, Docker Compose manifests.
//! Template instantiation: O(|V| + |E|).

use crate::genome::*;

/// Generate a complete project scaffold from a genome.
pub fn generate_scaffold(genome: &ArchGenome, project_name: &str) -> Scaffold {
    let mut files = Vec::new();

    // Generate service boilerplate for each vertex
    for service in &genome.services {
        let template = service_template(service);
        files.push(ScaffoldFile {
            path: format!("services/{}/main.py", service.name),
            content: template,
        });
        files.push(ScaffoldFile {
            path: format!("services/{}/requirements.txt", service.name),
            content: service_requirements(service),
        });
        files.push(ScaffoldFile {
            path: format!("services/{}/Dockerfile", service.name),
            content: dockerfile_template(&service.name),
        });
    }

    // Generate communication wiring config
    for channel in &genome.channels {
        if channel.from < genome.services.len() && channel.to < genome.services.len() {
            let from_name = &genome.services[channel.from].name;
            let to_name = &genome.services[channel.to].name;
            files.push(ScaffoldFile {
                path: format!("config/{}-to-{}.json", from_name, to_name),
                content: channel_config(channel, from_name, to_name),
            });
        }
    }

    // Docker Compose
    files.push(ScaffoldFile {
        path: "docker-compose.yml".to_string(),
        content: docker_compose(genome, project_name),
    });

    // README
    files.push(ScaffoldFile {
        path: "README.md".to_string(),
        content: readme(genome, project_name),
    });

    Scaffold {
        project_name: project_name.to_string(),
        files,
        total_services: genome.services.len(),
        total_channels: genome.channels.len(),
    }
}

/// A generated project scaffold.
#[derive(Debug, Clone)]
pub struct Scaffold {
    pub project_name: String,
    pub files: Vec<ScaffoldFile>,
    pub total_services: usize,
    pub total_channels: usize,
}

#[derive(Debug, Clone)]
pub struct ScaffoldFile {
    pub path: String,
    pub content: String,
}

fn service_template(service: &Service) -> String {
    match service.service_type {
        ServiceType::Gateway => format!(
            r#""""{name} — API Gateway"""
from fastapi import FastAPI

app = FastAPI(title="{name}")

@app.get("/health")
async def health():
    return {{"status": "ok", "service": "{name}"}}

@app.get("/")
async def root():
    return {{"service": "{name}", "type": "gateway"}}
"#,
            name = service.name
        ),
        ServiceType::Stateless => format!(
            r#""""{name} — Stateless Worker"""
from fastapi import FastAPI

app = FastAPI(title="{name}")

@app.get("/health")
async def health():
    return {{"status": "ok"}}

@app.post("/process")
async def process(data: dict):
    # Business logic here
    return {{"result": "processed", "input": data}}
"#,
            name = service.name
        ),
        ServiceType::Database => format!(
            r#""""{name} — Database Service"""
import sqlite3

DB_PATH = "/data/{name}.db"

def get_connection():
    return sqlite3.connect(DB_PATH)

def init_db():
    conn = get_connection()
    conn.execute("CREATE TABLE IF NOT EXISTS data (id TEXT PRIMARY KEY, value TEXT)")
    conn.commit()
    conn.close()

init_db()
"#,
            name = service.name
        ),
        ServiceType::Queue => format!(
            r#""""{name} — Message Queue Worker"""
import asyncio
import json

QUEUE = asyncio.Queue()

async def produce(message: dict):
    await QUEUE.put(json.dumps(message))

async def consume():
    while True:
        msg = await QUEUE.get()
        data = json.loads(msg)
        # Process message
        print(f"Processing: {{data}}")
        QUEUE.task_done()
"#,
            name = service.name
        ),
        _ => format!(
            r#""""{name} — Service"""
from fastapi import FastAPI

app = FastAPI(title="{name}")

@app.get("/health")
async def health():
    return {{"status": "ok"}}
"#,
            name = service.name
        ),
    }
}

fn service_requirements(service: &Service) -> String {
    match service.service_type {
        ServiceType::Gateway | ServiceType::Stateless | ServiceType::Stateful => "fastapi>=0.100\nuvicorn>=0.23\nhttpx>=0.24\n".to_string(),
        ServiceType::Database => "sqlite3\n".to_string(),
        ServiceType::Queue => "asyncio\n".to_string(),
        ServiceType::Cache => "redis>=5.0\n".to_string(),
    }
}

fn dockerfile_template(name: &str) -> String {
    format!(
        r#"FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY . .
CMD ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8000"]
"#
    )
}

fn channel_config(channel: &Channel, from: &str, to: &str) -> String {
    serde_json::json!({
        "from": from,
        "to": to,
        "type": format!("{:?}", channel.channel_type),
        "reliability": channel.reliability,
    }).to_string()
}

fn docker_compose(genome: &ArchGenome, project: &str) -> String {
    let mut compose = format!("# {} — Auto-generated from architecture genome\nversion: '3.8'\nservices:\n", project);

    for (i, service) in genome.services.iter().enumerate() {
        let port = 8000 + i;
        compose.push_str(&format!(
            "  {}:\n    build: ./services/{}\n    ports:\n      - \"{}:8000\"\n    restart: unless-stopped\n\n",
            service.name, service.name, port
        ));
    }

    compose
}

fn readme(genome: &ArchGenome, project: &str) -> String {
    let mut md = format!("# {}\n\nAuto-generated architecture scaffold.\n\n## Services\n\n", project);
    for svc in &genome.services {
        md.push_str(&format!("- **{}** ({:?}) — ${:.0}/month, {:.0}ms latency\n",
            svc.name, svc.service_type, svc.cost_estimate, svc.latency_estimate));
    }
    md.push_str("\n## Communication\n\n");
    for ch in &genome.channels {
        if ch.from < genome.services.len() && ch.to < genome.services.len() {
            md.push_str(&format!("- {} → {} ({:?})\n",
                genome.services[ch.from].name, genome.services[ch.to].name, ch.channel_type));
        }
    }
    md.push_str("\n## Quick Start\n\n```bash\ndocker-compose up --build\n```\n");
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scaffold_from_monolith() {
        let genome = ArchGenome::monolith("my-app");
        let scaffold = generate_scaffold(&genome, "my-project");
        assert!(scaffold.files.len() >= 4, "Should have service files + compose + readme: {}", scaffold.files.len());
        assert!(scaffold.files.iter().any(|f| f.path.contains("docker-compose")));
        assert!(scaffold.files.iter().any(|f| f.path.contains("main.py")));
    }

    #[test]
    fn test_scaffold_from_microservices() {
        let mut genome = ArchGenome::monolith("app");
        let mut rng = rand::thread_rng();
        genome.split_service(&mut rng);
        genome.split_service(&mut rng);

        let scaffold = generate_scaffold(&genome, "microservices");
        assert!(scaffold.total_services >= 3, "Should have 3+ services");
        let compose = scaffold.files.iter().find(|f| f.path == "docker-compose.yml").unwrap();
        assert!(compose.content.contains("services:"));
    }
}
