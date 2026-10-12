// A fake DynamoDB transport. ElectroDB gets a real v3 DocumentClient, so its
// requests go through the real lib-dynamodb marshalling and the real AWS JSON
// protocol. The HTTP layer is replaced: every request body is logged in wire
// form, and the response is the next scripted wire body.

import { DynamoDBClient } from "@aws-sdk/client-dynamodb";
import { DynamoDBDocumentClient } from "@aws-sdk/lib-dynamodb";

// Responses used once a case's scripted responses run out.
const defaultResponses = {
  GetItem: {},
  PutItem: {},
  UpdateItem: {},
  DeleteItem: {},
  Query: { Items: [], Count: 0, ScannedCount: 0 },
  Scan: { Items: [], Count: 0, ScannedCount: 0 },
  BatchGetItem: { Responses: {}, UnprocessedKeys: {} },
  BatchWriteItem: { UnprocessedItems: {} },
  TransactGetItems: { Responses: [] },
  TransactWriteItems: {},
};

export class FakeTransport {
  constructor() {
    this.reset([]);
  }

  // reset starts a new case: clears the request log and loads its responses.
  // A response body containing "__type" is returned as an HTTP 400 error.
  reset(responses) {
    this.requests = [];
    this.responses = [...responses];
  }

  async handle(request) {
    const target = request.headers["x-amz-target"].split(".").pop();
    const body = JSON.parse(Buffer.from(request.body).toString("utf8"));
    this.requests.push({ target, request: body });

    const scripted = this.responses.length > 0 ? this.responses.shift() : undefined;
    const response = scripted ?? defaultResponses[target] ?? {};
    const isError = Object.hasOwn(response, "__type");
    return {
      response: {
        statusCode: isError ? 400 : 200,
        headers: { "content-type": "application/x-amz-json-1.0" },
        body: Buffer.from(JSON.stringify(response)),
      },
    };
  }

  updateHttpClientConfig() {}

  httpHandlerConfigs() {
    return {};
  }
}

// newDocumentClient returns a DocumentClient with default marshall options
// whose requests go to transport.
export function newDocumentClient(transport) {
  const client = new DynamoDBClient({
    region: "us-east-1",
    endpoint: "http://localhost:8000",
    credentials: { accessKeyId: "fake", secretAccessKey: "fake" },
    maxAttempts: 1,
    requestHandler: transport,
  });
  return DynamoDBDocumentClient.from(client);
}
